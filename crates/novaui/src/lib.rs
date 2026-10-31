//! NovaUI: a retained, damage-driven UI framework. See `docs/novaui-spec.md`.

mod access;
pub mod anim;
pub mod app;
mod audit;
#[cfg(any(debug_assertions, test))]
mod dev;
pub mod dsl;
pub mod element;
mod error;
mod focus;
mod font_scan;
mod geom;
pub mod i18n;
pub mod image;
pub mod input;
pub mod model;
pub mod platform;
pub(crate) mod render;
mod runner;
pub mod sched;
pub mod semantics;
mod style;
pub mod text;
pub mod theme;
pub mod trace;
mod vector;
pub mod widgets;

pub use anim::{Ease, Pose, Prop, Then};
pub use app::{Action, App, Event, FilePick, NcHit, Popup, Ui};
pub use element::{Id, Listen, State, Tree};
pub use error::Error;
pub use font_scan::{system_fonts, SystemFont};
pub use geom::*;
pub use i18n::Strings;
pub use image::ImageHandle;
pub use input::{Input, Mods, MouseButton, Pointer};
pub use model::Model;
pub use nova_icons::Icon;
pub use render::primitive::corner;
pub use sched::{Cancel, Clock, Message, Priority, Queue, Subscription, Task, Ticker, Timer, Waker};
pub use semantics::{Range, Role, Semantics};
pub use style::*;
pub use text::Align;
pub use vector::Shape;

/// Renderers for headless drivers (screenshots, tests): the `Backend` a frame is drawn into and
/// the CPU one. Windows draw through their platform's backend; nothing else needs these.
#[cfg(feature = "backend")]
pub mod backend {
    #[cfg(windows)]
    pub use crate::render::d3d11::D3d11;
    pub use crate::render::primitive::{Draw, HdrOutput, Prim, Target};
    pub use crate::render::software::Software;
    #[cfg(not(windows))]
    pub use crate::render::vulkan::Vulkan;
    pub use crate::render::Backend;

    /// Draws the tree's pending frame into `be` (what a platform host does each frame).
    pub fn frame(tree: &mut crate::Tree, be: &mut dyn Backend, now: f64) -> bool {
        tree.frame(be, now)
    }
}

#[cfg(test)]
extern crate self as novaui;
#[cfg(test)]
mod internal_tests;
