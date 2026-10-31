//! The renderers: display lists (`primitive`) drawn by a `Backend`. `batch` encodes primitives
//! into instanced draws for the GPU backends (`vulkan`, `d3d11`); `software` draws on the CPU
//! (headless runs, screenshots). Crate-private; with the `backend` feature `Backend` and
//! `Software` are public for headless drivers (`novaui::backend`).

// Parts of the backends only headless drivers reach (readback, names) are unused without it.
#![cfg_attr(not(feature = "backend"), allow(dead_code, unused_imports))]

pub mod batch;
#[cfg(windows)]
pub mod d3d11;
pub mod primitive;
pub mod software;
/// Vulkan: Linux's renderer. Not on Windows, whose renderer is D3D11 (and the CPU one when it
/// cannot be had): a Windows program carries no second GPU renderer nor its shaders.
#[cfg(not(windows))]
pub mod vulkan;

pub use primitive::Backend;
