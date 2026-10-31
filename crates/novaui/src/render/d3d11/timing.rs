//! How long the GPU took for each frame (`Backend::gpu_frames`, for `NOVAUI_TRACE`): a pair of
//! timestamp queries around the frame's work inside a disjoint query, read back frames later
//! without waiting. Off (no queries) until a trace asks for it.

use std::collections::VecDeque;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::*;

/// One frame's queries: disjoint, start, end.
type Set = (ID3D11Query, ID3D11Query, ID3D11Query);

/// Frames in flight at most; a frame begun with more still unread is not timed.
const IN_FLIGHT: usize = 8;

#[derive(Default)]
pub(super) struct Timing {
    on: bool,
    open: Option<Set>,
    pending: VecDeque<Set>,
    spare: Vec<Set>,
}

impl Timing {
    pub(super) fn set_on(&mut self, on: bool) {
        self.on = on;
        if !on {
            self.open = None;
            self.pending.clear();
            self.spare.clear();
        }
    }

    /// The frame's first GPU work is about to be issued.
    pub(super) fn begin(&mut self, dev: &ID3D11Device, ctx: &ID3D11DeviceContext) {
        if !self.on || self.open.is_some() || self.pending.len() >= IN_FLIGHT {
            return;
        }
        let Some(set) = self.spare.pop().or_else(|| make(dev)) else { return };
        // SAFETY: the immediate context on its thread; the queries are this device's.
        unsafe {
            ctx.Begin(&set.0);
            ctx.End(&set.1);
        }
        self.open = Some(set);
    }

    /// The frame's drawing is all issued (before its present; at its end without one).
    pub(super) fn end(&mut self, ctx: &ID3D11DeviceContext) {
        if let Some(set) = self.open.take() {
            // SAFETY: as in `begin`.
            unsafe {
                ctx.End(&set.2);
                ctx.End(&set.0);
            }
            self.pending.push_back(set);
        }
    }

    /// Adds the µs of each frame the GPU has finished since the last call, oldest first.
    pub(super) fn finished(&mut self, ctx: &ID3D11DeviceContext, out: &mut Vec<u32>) {
        while let Some(set) = self.pending.front() {
            let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            let (mut t0, mut t1) = (0u64, 0u64);
            if !(ready(ctx, &set.0, &mut dj) && ready(ctx, &set.1, &mut t0) && ready(ctx, &set.2, &mut t1)) {
                break;
            }
            if !dj.Disjoint.as_bool() && dj.Frequency > 0 && t1 >= t0 {
                out.push(((t1 - t0) as u128 * 1_000_000 / u128::from(dj.Frequency)).min(u32::MAX as u128) as u32);
            }
            let set = self.pending.pop_front().expect("the front was just read");
            self.spare.push(set);
        }
    }
}

fn make(dev: &ID3D11Device) -> Option<Set> {
    let q = |kind| {
        let mut out = None;
        // SAFETY: a live descriptor and out-parameter for the call.
        unsafe { dev.CreateQuery(&D3D11_QUERY_DESC { Query: kind, MiscFlags: 0 }, Some(&mut out)) }.ok()?;
        out
    };
    Some((q(D3D11_QUERY_TIMESTAMP_DISJOINT)?, q(D3D11_QUERY_TIMESTAMP)?, q(D3D11_QUERY_TIMESTAMP)?))
}

/// Reads `q`'s result into `out` if the GPU has it, without flushing or waiting. The binding's
/// `GetData` folds S_FALSE ("not yet") into Ok, so the call goes through the vtable.
fn ready<T>(ctx: &ID3D11DeviceContext, q: &ID3D11Query, out: &mut T) -> bool {
    // SAFETY: `out` is a live `T` of the size passed, the query and context are this device's,
    // on the context's thread; the vtable entry is the interface's own GetData.
    let hr = unsafe {
        (Interface::vtable(ctx).GetData)(
            Interface::as_raw(ctx),
            Interface::as_raw(q),
            out as *mut T as *mut core::ffi::c_void,
            std::mem::size_of::<T>() as u32,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )
    };
    hr.0 == 0
}
