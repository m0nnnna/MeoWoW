//! Item icons: turning an entry off the wire into something to draw.
//!
//! The bag window needs a picture per slot; the network only ever sends an item
//! *entry*. Two tables bridge that, and neither is optional: `Item.dbc` maps an
//! entry to a display id, and `ItemDisplayInfo` maps a display id to an icon
//! name. Both are already transcribed and both were identified against controls
//! rather than from memory -- see their doc comments in `dbc::schema`.
//!
//! **The icon column holds a bare name, not a path.** `ItemDisplayInfo` stores
//! `INV_Fabric_Linen_01` where `SpellIcon` stores a full
//! `Interface\Icons\Spell_...`, so the two are not interchangeable and this
//! module adds the directory and the extension. That is a small enough
//! difference to be assumed and it was not: the first path built the spell way
//! failed to resolve, and the check that settled it was reading six real icons
//! by path for items the test character is actually carrying.
//!
//! **What is missing here is the name.** Item names live on the server, not in
//! `Item.dbc`, so naming a slot needs an item query this client does not speak
//! yet. Rather than invent a name, a slot without one falls back to its entry,
//! which is honest and looks like what it is -- the same rule the description
//! substituter follows when it leaves a `$s1` visible rather than fabricating a
//! number.

use std::collections::HashMap;

use mpq::Chain;
use render::{Gpu, UploadedTexture};

/// Where item icons live. See the module comment for why this is not the same
/// shape as a spell icon's stored path.
const ICON_DIR: &str = r"Interface\Icons";

/// Item icons, resolved and uploaded on demand.
#[derive(Default)]
pub struct Items {
    /// Entry to icon path, for every item in the game.
    ///
    /// Built in one pass rather than looked up per entry, because a lookup
    /// means parsing a forty-six-thousand-row table and then a
    /// fifty-eight-thousand-row one -- doing that per slot would re-read both
    /// tables sixteen times to fill one bag window. The tables are read once,
    /// at the same point the spellbook is loaded, and this map is what
    /// survives them.
    paths: HashMap<u32, String>,
    /// Entry to `(display id, inventory type)`, for dressing *other* players.
    /// The wire only ever gives their gear as entries -- see
    /// `world::state::Entity::visible_item_entries` -- and this is the same
    /// `Item.dbc` pass that builds `paths`, so it costs nothing extra to keep.
    display: HashMap<u32, (u32, u8)>,
    /// Uploaded icons, by texture path. Many items share one icon -- every
    /// stack of linen cloth in the game is the same picture -- so this is
    /// keyed by path rather than by entry.
    icons: HashMap<String, Option<egui::TextureId>>,
    /// Kept alive because egui holds only the id: dropping the texture would
    /// leave the interface drawing a freed view.
    uploaded: Vec<UploadedTexture>,
}

impl Items {
    /// Reads `Item.dbc` and `ItemDisplayInfo` and builds the entry-to-icon map.
    ///
    /// Infallible in the same way the rest of the interface is: with no game
    /// installation there are no icons, the map is empty, and every bag slot
    /// falls back to text. A client that refused to show an inventory without
    /// artwork would be a worse client.
    pub fn load(chain: &mut Chain) -> Self {
        use dbc::schema::{Item, ItemDisplayInfo};

        let started = std::time::Instant::now();
        let mut items = Items::default();

        // Display id to icon name first, so the item table is walked once
        // against a map that is already complete.
        let icons: HashMap<u32, String> = chain
            .read(ItemDisplayInfo::PATH)
            .ok()
            .and_then(|bytes| ItemDisplayInfo::parse(&bytes).ok())
            .map(|table| {
                table
                    .iter()
                    // An empty column is a real answer -- plenty of display
                    // rows have no icon -- and must not become the path
                    // `Interface\Icons\.blp`, which would then be read and
                    // fail once per entry that shares it.
                    .filter(|row| !row.inventory_icon().is_empty())
                    .map(|row| (row.id(), row.inventory_icon().to_string()))
                    .collect()
            })
            .unwrap_or_default();

        if let Some(table) = chain
            .read(Item::PATH)
            .ok()
            .and_then(|bytes| Item::parse(&bytes).ok())
        {
            for row in table.iter() {
                if let Some(icon) = icons.get(&row.display_info_id()) {
                    items
                        .paths
                        .insert(row.id(), format!("{ICON_DIR}\\{icon}.blp"));
                }
                items
                    .display
                    .insert(row.id(), (row.display_info_id(), row.inventory_type() as u8));
            }
        }

        tracing::info!(
            "item icons loaded in {:?}: {} entries over {} icons",
            started.elapsed(),
            items.paths.len(),
            icons.len()
        );
        items
    }

    /// The icon for an item entry, uploading it the first time it is needed.
    ///
    /// Failures are cached as `None`: a BLP that will not load will not load
    /// on the next frame either, and retrying every frame would hitch forever
    /// over one bad path. Same reasoning as `spells::Spellbook::icon`, which
    /// this deliberately mirrors.
    pub fn icon(
        &mut self,
        gpu: &Gpu,
        renderer: &mut egui_wgpu::Renderer,
        chain: &mut Chain,
        entry: u32,
    ) -> Option<egui::TextureId> {
        let path = self.paths.get(&entry)?.clone();
        if let Some(cached) = self.icons.get(&path) {
            return *cached;
        }

        let id = (|| {
            let bytes = chain.read(&path).ok()?;
            let image = blp::Blp::parse(&bytes).ok()?;
            let uploaded = render::texture::upload_blp(gpu, &image, &path);
            let id = renderer.register_native_texture(
                &gpu.device,
                &uploaded.view,
                wgpu::FilterMode::Linear,
            );
            self.uploaded.push(uploaded);
            Some(id)
        })();
        if id.is_none() {
            tracing::debug!("no icon at {path}");
        }
        self.icons.insert(path, id);
        id
    }

    /// `(display id, inventory type)` for an item entry, the shape
    /// `character::resolve_wearing` wants. `None` for an entry `Item.dbc`
    /// does not have, which happens and is not an error -- see
    /// `Item.dbc`'s own doc comment.
    pub fn display(&self, entry: u32) -> Option<(u32, u8)> {
        self.display.get(&entry).copied()
    }

    /// What to call an item, until this client can ask the server.
    ///
    /// See the module comment: the entry is shown rather than a made-up name,
    /// so a slot with no icon says something checkable instead of something
    /// plausible.
    pub fn name(&self, entry: u32) -> String {
        format!("Item {entry}")
    }
}
