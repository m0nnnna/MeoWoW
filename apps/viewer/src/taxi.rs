//! The flight network, read out of the client's own tables.
//!
//! **Almost nothing about a flight comes off the wire.** `SMSG_SHOWTAXINODES`
//! says which nodes this character may use and which one it is standing at,
//! as a bit array and an id -- no names, no positions, no routes and no
//! prices. Every one of those lives in `TaxiNodes`, `TaxiPath` and
//! `TaxiPathNode`, which is why 4.25 measured the tables before it sent a
//! packet.
//!
//! This module turns the three tables into the one question the interface
//! actually asks: *standing here, where can I go and what does it cost?*

use std::collections::HashMap;

use mpq::Chain;

/// One destination reachable from wherever the player is standing.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    /// The `TaxiNodes` row to ask for.
    pub node: u32,
    pub name: String,
    /// Copper, from `TaxiPath.cost`.
    ///
    /// **Unlike a vendor's price or a trainer's, this one is the table's and
    /// not the wire's** -- the server sends no price with the menu at all. So
    /// it is the one number in this milestone that has had no reputation
    /// discount applied, and a client that showed it as authoritative would
    /// be guessing. Shown because it is what the tables say and a blank is
    /// worse; the purse is what settles it.
    pub cost: u32,
}

/// The flight network as the client's tables describe it.
#[derive(Default)]
pub struct Network {
    /// Node id to its player-facing name.
    names: HashMap<u32, String>,
    /// Departure node to everything reachable directly from it.
    ///
    /// Keyed by departure because that is the only question ever asked: a
    /// flight master offers the routes leaving the node it serves. Building
    /// the reverse index too would be two structures kept in step for a
    /// question nobody poses.
    routes: HashMap<u32, Vec<(u32, u32)>>,
}

impl Network {
    /// Reads `TaxiNodes.dbc` and `TaxiPath.dbc`.
    ///
    /// Infallible like the rest of the interface: with no game installation
    /// the network is empty, every node resolves to no name, and the flight
    /// window says the master has nothing to offer rather than refusing to
    /// open. `TaxiPathNode` is deliberately **not** read here -- the waypoints
    /// describe the ride, and the ride is the server's spline. See
    /// [`::world::Flight`] for why the client must not prefer its own copy.
    pub fn load(chain: &mut Chain) -> Self {
        use dbc::schema::{TaxiNodes, TaxiPath};

        let started = std::time::Instant::now();
        let mut network = Network::default();
        if let Ok(bytes) = chain.read(TaxiNodes::PATH) {
            if let Ok(table) = TaxiNodes::parse(&bytes) {
                for row in table.iter() {
                    network.names.insert(row.id(), row.name().to_string());
                }
            }
        }
        if let Ok(bytes) = chain.read(TaxiPath::PATH) {
            if let Ok(table) = TaxiPath::parse(&bytes) {
                for row in table.iter() {
                    network
                        .routes
                        .entry(row.from_node())
                        .or_default()
                        .push((row.to_node(), row.cost()));
                }
            }
        }
        tracing::info!(
            "flight network loaded in {:?}: {} node(s), {} departure point(s)",
            started.elapsed(),
            network.names.len(),
            network.routes.len()
        );
        network
    }

    /// What a node is called, or `None` when the tables do not say.
    pub fn name(&self, node: u32) -> Option<&str> {
        self.names.get(&node).map(String::as_str)
    }

    /// Everywhere the player may fly from `from`, given what the server says
    /// they know.
    ///
    /// **Both filters matter and they are different questions.** `TaxiPath`
    /// says a route physically exists; the mask says this character has
    /// visited the far end. Offering a route the mask excludes produces a
    /// refusal the player cannot understand, and hiding one the mask allows
    /// loses a destination -- so the two are applied together and neither is
    /// inferred from the other.
    ///
    /// Sorted by name so the list is stable between visits. Row order in
    /// `TaxiPath` is by path id, which is neither alphabetical nor
    /// geographic, and a list that reshuffled itself would make the player
    /// re-read it every time.
    pub fn destinations(&self, from: u32, known: &::world::TaxiMenu) -> Vec<Destination> {
        let mut out: Vec<Destination> = self
            .routes
            .get(&from)
            .into_iter()
            .flatten()
            .filter(|(to, _)| known.knows(*to))
            // A route back to where you already stand is in the table for
            // some nodes and is not an offer. The server refuses it, so
            // drawing it would be a row whose only outcome is an error.
            .filter(|(to, _)| *to != from)
            .map(|(to, cost)| Destination {
                node: *to,
                name: self
                    .names
                    .get(to)
                    .cloned()
                    .unwrap_or_else(|| format!("Node {to}")),
                cost: *cost,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.node.cmp(&b.node)));
        out.dedup_by(|a, b| a.node == b.node);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(known: &[u32]) -> ::world::TaxiMenu {
        let mut mask = [0u32; ::world::taxi::MASK_WORDS];
        for node in known {
            mask[*node as usize / 32] |= 1 << (node % 32);
        }
        ::world::TaxiMenu {
            npc: 1,
            current_node: 2,
            known: mask,
            unknown: 1,
        }
    }

    fn network() -> Network {
        let mut n = Network::default();
        for (id, name) in [(2, "Stormwind"), (4, "Sentinel Hill"), (6, "Ironforge")] {
            n.names.insert(id, name.to_string());
        }
        n.routes.insert(2, vec![(4, 100), (6, 250), (2, 0)]);
        n
    }

    /// **The mask filters the list.** A route the tables describe but the
    /// character has never visited is not an offer -- drawing it produces a
    /// refusal with no explanation the player can see.
    #[test]
    fn only_known_destinations_are_offered() {
        let n = network();
        let all = n.destinations(2, &menu(&[2, 4, 6]));
        assert_eq!(all.len(), 2, "two real destinations");
        let some = n.destinations(2, &menu(&[2, 4]));
        assert_eq!(some.len(), 1);
        assert_eq!(some[0].name, "Sentinel Hill");
        assert_eq!(some[0].cost, 100);
    }

    /// A route back to where you stand is in the table and is not an offer.
    #[test]
    fn the_node_you_are_standing_at_is_not_a_destination() {
        let n = network();
        assert!(n.destinations(2, &menu(&[2, 4, 6])).iter().all(|d| d.node != 2));
    }

    /// Sorted by name, so the list does not reshuffle between visits.
    #[test]
    fn the_list_is_stable() {
        let n = network();
        let list = n.destinations(2, &menu(&[2, 4, 6]));
        let names: Vec<&str> = list.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["Ironforge", "Sentinel Hill"]);
    }

    /// No tables at all is an empty list, not a panic -- the interface has to
    /// come up without a game installation.
    #[test]
    fn an_empty_network_offers_nothing() {
        let n = Network::default();
        assert!(n.destinations(2, &menu(&[2, 4])).is_empty());
        assert_eq!(n.name(2), None);
    }

    /// A node with no name still travels, labelled by its id. The alternative
    /// is dropping a destination the character can actually reach because the
    /// table happened to leave its name blank.
    #[test]
    fn a_nameless_node_is_still_offered() {
        let mut n = network();
        n.names.remove(&4);
        let list = n.destinations(2, &menu(&[2, 4]));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Node 4");
    }
}
