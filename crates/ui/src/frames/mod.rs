//! The things the interface actually draws.
//!
//! Each module here owns one kind of frame and exposes the same pair: a
//! `size` telling the layout how much room it wants at a given scale, and a
//! `draw` painting it into the rectangle the layout chose. Nothing in here
//! knows where it is on screen or how it got there -- that is
//! [`crate::element`]'s job -- and nothing in here reads the network.
//!
//! Frames are painted from explicit geometry rather than built from egui
//! widgets. That costs a little more code per frame and buys the thing this
//! design is for: `scale` genuinely multiplies every dimension, and the
//! appearance is a function of [`crate::style::Style`] alone, so a user who
//! rewrites the style file gets a different-looking client rather than egui
//! with different colours.

pub mod action_bar;
pub mod bags;
pub mod cast_bar;
pub mod character;
pub mod chat;
pub mod loot;
pub mod combat_text;
pub mod marker;
pub mod release;
pub mod spellbook;
pub mod unit;

pub use bags::{BagItem, BagSlot};
pub use character::EquipSlot;
pub use loot::{LootRow, Take};
pub use cast_bar::CastBarView;
pub use release::ReleasePromptView;
pub use spellbook::SpellbookEntry;
pub use unit::UnitView;
