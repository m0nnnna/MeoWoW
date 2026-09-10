//! Loading entity models off the render thread.
//!
//! **The problem this exists for.** The first time a creature's look comes into
//! view, `World::set_entities` reads its `.m2`, forty-odd `.anim` files and its
//! textures out of the archives, decodes them and uploads them -- 20 to 50 ms,
//! synchronously, inside the frame. In a city the portal walk hides most of
//! that behind walls; outdoors in Elwynn a wolf trotting over a rise stalls the
//! whole frame for everyone. The `LoadProbe` instrument in [`crate::world`]
//! priced it: `entities 41.6 (ground 0.2)` is really ~2.5 ms of instance
//! rebuild behind a ~39 ms cold load of three creatures that just appeared.
//!
//! **What moves.** [`model::prepare_dressed_with`] -- every archive read, every
//! parse, the skeleton decode -- runs on a background thread with its own
//! `Chain` and [`model::Sources`]. The render thread keeps only
//! [`model::finalize_dressed`], one mesh upload and a handful of small texture
//! uploads, which it does when the result arrives a frame or two later. The
//! creature appears a beat late instead of freezing the frame.
//!
//! **Scope.** Creature, NPC and player bodies only -- the `entity_model` miss
//! path. Held weapons and game objects stay synchronous: smaller, rarer, and
//! mostly already warm in the tile loader's cache.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use mpq::Chain;

use crate::character::{Look, NpcAppearances};
use crate::model::{self, PreparedModel, Sources};

/// A `(display_id, look_key)` pair -- the `entity_cache` key in [`crate::world`].
pub type Key = (u32, u64);

struct Request {
    key: Key,
    display_id: u32,
    /// A player's own dress, cloned off the character list. `None` for an NPC,
    /// whose body texture the worker resolves from `CreatureDisplayInfoExtra`
    /// -- the same lookup `World::npc_look` does, moved to the worker so that
    /// read is off the frame too.
    look: Option<Look>,
    lod: u32,
}

/// One finished load, ready for [`model::finalize_dressed`] on the render
/// thread.
pub struct Response {
    pub key: Key,
    /// `None` when the load failed -- a broken path, an unreadable skin. The
    /// caller caches that as a permanent miss, exactly as the synchronous path
    /// caches `None`, so a genuinely absent model is not re-requested every
    /// frame.
    pub prepared: Option<PreparedModel>,
}

/// A background thread that turns display ids into [`PreparedModel`]s.
///
/// One worker is enough: outdoors, new looks arrive seconds apart as the player
/// walks, and a single 40 ms load between them is invisible off the frame. A
/// pool is a `for` loop away if a zone is ever found that needs it.
pub struct ModelLoader {
    req_tx: Sender<Request>,
    res_rx: Receiver<Response>,
    /// Keys asked for and not yet returned, so a request already in flight is
    /// not sent again on every frame the creature is still loading.
    in_flight: HashSet<Key>,
    /// Dropped last; dropping `req_tx` ends the worker's `recv` loop.
    _worker: JoinHandle<()>,
}

impl ModelLoader {
    /// Starts the worker, giving it its own archive handles.
    ///
    /// Fails only if the archives cannot be opened -- in which case the caller
    /// keeps loading synchronously, which is exactly today's behaviour.
    pub fn spawn(data_dir: PathBuf, locale: String) -> anyhow::Result<Self> {
        // Its own `Chain`: `Chain::read` needs `&mut self`, so sharing one with
        // the render thread means a lock on every archive read on both sides --
        // tile streaming included. Reopening pays for the archive headers once,
        // here, off the render thread, and then never contends.
        let chain = Chain::open_wow_data(&data_dir, &locale)?;

        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<Response>();

        let worker = std::thread::Builder::new()
            .name("model-loader".to_owned())
            .spawn(move || {
                let mut chain = chain;
                let mut sources = Sources::default();
                // `Option<Option<_>>`: outer is "have we tried", inner is the
                // table itself or `None` for a broken install -- the same
                // shape `World` uses, so a missing table is reported once
                // rather than retried per creature.
                let mut npc: Option<Option<NpcAppearances>> = None;
                while let Ok(req) = req_rx.recv() {
                    let prepared = load_one(&mut chain, &mut sources, &mut npc, &req);
                    if res_tx
                        .send(Response { key: req.key, prepared })
                        .is_err()
                    {
                        // The world is being torn down; nothing left to answer.
                        break;
                    }
                }
            })?;

        Ok(Self {
            req_tx,
            res_rx,
            in_flight: HashSet::new(),
            _worker: worker,
        })
    }

    /// Asks for a model, unless one for this key is already being loaded.
    pub fn request(&mut self, key: Key, display_id: u32, look: Option<Look>, lod: u32) {
        if !self.in_flight.insert(key) {
            return;
        }
        if self
            .req_tx
            .send(Request { key, display_id, look, lod })
            .is_err()
        {
            // Worker gone: drop the reservation so a later attempt is not
            // swallowed. There is nothing else to be done from here.
            self.in_flight.remove(&key);
        }
    }

    /// Everything the worker has finished since the last call. Non-blocking.
    pub fn drain(&mut self) -> Vec<Response> {
        let mut done = Vec::new();
        loop {
            match self.res_rx.try_recv() {
                Ok(resp) => {
                    self.in_flight.remove(&resp.key);
                    done.push(resp);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        done
    }
}

/// Resolve the look, the model path and the geometry -- everything but the GPU.
fn load_one(
    chain: &mut Chain,
    sources: &mut Sources,
    npc: &mut Option<Option<NpcAppearances>>,
    req: &Request,
) -> Option<PreparedModel> {
    let npc_look = if req.look.is_none() {
        npc.get_or_insert_with(|| NpcAppearances::load(chain))
            .as_ref()
            .and_then(|n| n.look(req.display_id))
    } else {
        None
    };
    let look = req.look.as_ref().or(npc_look.as_ref());

    let (path, variations) = model::creature(sources, chain, req.display_id)
        .map_err(|e| tracing::debug!("display {}: {e:#}", req.display_id))
        .ok()?;
    model::prepare_dressed_with(chain, sources, &path, &variations, req.lod, look)
        .map_err(|e| tracing::debug!("display {}: {e:#}", req.display_id))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loader reads a real creature on its own thread, and a second
    /// request for a key already in flight is suppressed rather than sent
    /// twice -- the thing that keeps `set_entities` from re-asking every
    /// frame. Skipped without `WOW_DATA`.
    #[test]
    fn a_creature_loads_off_thread_and_is_not_requested_twice() {
        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut loader = ModelLoader::spawn(PathBuf::from(data), "enUS".to_owned())
            .expect("starting the loader");

        let key = (1859u32, 0u64); // Marshal McBride -- HumanMale.m2
        loader.request(key, key.0, None, 0);
        loader.request(key, key.0, None, 0); // already in flight: no second send

        let mut done = Vec::new();
        for _ in 0..200 {
            done.extend(loader.drain());
            if !done.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Let a stray duplicate arrive before counting.
        std::thread::sleep(std::time::Duration::from_millis(100));
        done.extend(loader.drain());

        assert_eq!(done.len(), 1, "one request in flight, one response");
        assert_eq!(done[0].key, key);
        let prepared = done[0].prepared.as_ref().expect("a real creature prepared");
        assert!(!prepared.draws.is_empty(), "the model has draw calls");
        assert!(!prepared.bones.is_empty(), "and an animated skeleton");

        // Draining freed the key: a fresh request is sent, not swallowed.
        loader.request(key, key.0, None, 0);
        for _ in 0..200 {
            let again = loader.drain();
            if let Some(resp) = again.into_iter().next() {
                assert_eq!(resp.key, key);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("a re-request after draining never came back");
    }
}
