//! Opt-in buffer snapshots at selected compute dispatches. Unlike title
//! memory dumped at present, these contain the host bytes at the command.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;

use crate::host::{HostList, host, transition};

pub(crate) fn heap_indices() -> &'static [u32] {
    static INDICES: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    INDICES.get_or_init(|| {
        std::env::var("VICTORY_CAPTURE_BUFFER_HEAP")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    })
}

struct Source {
    buffer: ID3D12Resource,
    at: u64,
    bytes: u64,
    title: u64,
    label: String,
    note: String,
    large: bool,
}

/// Inputs and outputs of one selected dispatch. Paths contain a sequence
/// number so repeated dispatches with identical bindings cannot overwrite.
pub(crate) struct Dispatch {
    folder: PathBuf,
    prefix: String,
    note: String,
    sources: Vec<Source>,
    frame: u64,
}

impl Dispatch {
    pub(crate) fn new(hash: &str) -> Option<Self> {
        if !selected(hash) {
            return None;
        }
        let frame = crate::queue::presented() + 1;
        if !super::listed(frame) {
            return None;
        }
        let folder = super::folder()?.join(format!("frame-{frame}-buffers"));
        let sequence = next();
        Some(Self {
            folder,
            prefix: format!("{sequence:06}-{hash}"),
            note: format!("frame={frame} dispatch={sequence} shader={hash}"),
            sources: Vec::new(),
            frame,
        })
    }

    pub(crate) fn constants(&self, list: &HostList, slot: usize, words: &[u32]) {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let path = self
            .folder
            .join(format!("{}-constants-s{slot}.bin", self.prefix));
        let note = format!("{} binding=constants-s{slot}", self.note);
        list.defer_host(move || write(&path, &bytes, &note));
    }

    pub(crate) fn add(
        &mut self,
        heap: &crate::descriptor::host::HostHeap,
        index: u32,
        view: &crate::descriptor::View,
        label: String,
    ) {
        let Some((shadow, title, bytes)) = crate::descriptor::host::buffer_copy(view) else {
            return;
        };
        let Some(buffer) = heap.bound_buffer(index) else {
            return;
        };
        if heap.shows(index, &shadow.buffer) != Some(true) {
            // The actual slot's offset cannot safely be inferred from a
            // different shadow. Preserve the mismatch as evidence.
            let path = self
                .folder
                .join(format!("{}-{label}-mismatch.txt", self.prefix));
            let text = format!(
                "{} binding={label} heap={index} title={title:#x} bytes={bytes} descriptor_matches=false\n{}\n",
                self.note,
                heap.describe_slot(index).unwrap_or_default()
            );
            let _ = std::fs::create_dir_all(&self.folder);
            let _ = std::fs::write(path, text);
            return;
        }
        let large = bytes > 4 << 20;
        if large && !first_large(self.frame, shadow.id(), title, bytes) {
            return;
        }
        self.sources.push(Source {
            at: shadow.offset(title), buffer, bytes, title, label: label.clone(), large,
            note: format!("{} binding={label} heap={index} title={title:#x} bytes={bytes} shadow={} descriptor_matches=true", self.note, shadow.id()),
        });
    }

    pub(crate) fn record(&self, list: &HostList, after: bool) {
        let phase = if after { "after" } else { "before" };
        for source in &self.sources {
            if after && source.large {
                continue;
            }
            // Bound capture memory even if an arbitrary selected shader has
            // thousands of dispatches. Report incomplete captures explicitly.
            static BUDGET: Mutex<(u64, u64)> = Mutex::new((0, 0));
            {
                let mut budget = BUDGET.lock().unwrap_or_else(|e| e.into_inner());
                if budget.0 != self.frame {
                    *budget = (self.frame, 0);
                }
                if budget.1 + source.bytes > 512 << 20 {
                    tracing::warn!(frame = self.frame, binding = %source.label, "buffer capture exceeds 512 MiB per frame; omitted");
                    continue;
                }
                budget.1 += source.bytes;
            }
            let stem = format!("{}-{}-{phase}", self.prefix, source.label);
            let note = format!("{} phase={phase} host_offset={:#x}", source.note, source.at);
            Snapshot::record(
                list,
                &source.buffer,
                source.at,
                source.bytes,
                self.folder.join(format!("{stem}.bin")),
                note.clone(),
            );
            if !after {
                // This is CPU title memory at submission preparation, not
                // necessarily at the GPU command (earlier GPU writes may
                // still produce it). The label keeps that distinction clear.
                let path = self.folder.join(format!("{stem}-title-at-submit.bin"));
                let (title, bytes) = (source.title, source.bytes as usize);
                list.defer_host(move || {
                    let mut data = vec![0u8; bytes];
                    let readable = crate::host::read_into(title as *const u8, &mut data);
                    if readable {
                        write(&path, &data, &format!("{note} origin=title-at-submit"));
                    } else {
                        tracing::warn!(
                            title,
                            bytes,
                            "title buffer capture contains unreadable pages; omitted"
                        );
                    }
                });
            }
        }
    }
}

/// Comma-separated shader hashes; nothing is captured by default.
pub(crate) fn selected(hash: &str) -> bool {
    static HASHES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    HASHES
        .get_or_init(|| {
            std::env::var("VICTORY_CAPTURE_BUFFERS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .iter()
        .any(|h| h == hash)
}

/// One pair of before/after snapshots, named independently of list pointers.
pub(crate) fn next() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Relaxed)
}

/// Large heap inputs are copied once per listed frame and range. Small
/// batches and output buffers are copied at every selected dispatch.
pub(crate) fn first_large(frame: u64, copy: u64, address: u64, bytes: u64) -> bool {
    static SEEN: Mutex<(u64, Vec<(u64, u64, u64)>)> = Mutex::new((0, Vec::new()));
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.0 != frame {
        *seen = (frame, Vec::new());
    }
    let key = (copy, address, bytes);
    if seen.1.contains(&key) {
        return false;
    }
    seen.1.push(key);
    true
}

pub(crate) struct Snapshot {
    buffer: ID3D12Resource,
    bytes: usize,
    path: PathBuf,
    note: String,
}

impl Snapshot {
    /// Copies a COMMON-state host buffer, restoring COMMON. No refresh or
    /// descriptor mirroring: observing a stale input must not repair it.
    pub(crate) fn record(
        list: &HostList,
        source: &ID3D12Resource,
        at: u64,
        bytes: u64,
        path: PathBuf,
        note: String,
    ) {
        // SAFETY: a live buffer; reject ranges outside it.
        let width = unsafe { source.GetDesc() }.Width;
        if bytes == 0 || at.checked_add(bytes).is_none_or(|end| end > width) {
            tracing::warn!(at, bytes, width, "buffer capture range outside host buffer");
            return;
        }
        let Some(host) = host() else { return };
        let Some(buffer) = host.read_back_buffer(bytes) else {
            return;
        };
        // SAFETY: live buffers and an open list. The barrier orders shader
        // writes before the copy. All title buffer shadows start in COMMON
        // and use implicit buffer promotion, as the indirect clamp does.
        unsafe {
            list.list.ResourceBarrier(&[
                crate::indirect::uav_barrier(),
                transition(
                    source,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                ),
            ]);
            list.list.CopyBufferRegion(&buffer, 0, source, at, bytes);
            list.list.ResourceBarrier(&[transition(
                source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
        list.keep(source);
        list.defer_buffer_capture(Self {
            buffer,
            bytes: bytes as usize,
            path,
            note,
        });
    }

    /// Called by the submission's completion path, never by a frame-number
    /// guess. There is no ring to wrap or overwrite while the GPU is using it.
    pub(crate) fn save(&self) {
        let mut data = std::ptr::null_mut();
        // SAFETY: the submission fence completed; this readback has bytes.
        unsafe {
            if self.buffer.Map(0, None, Some(&mut data)).is_err() || data.is_null() {
                return;
            }
            let bytes = std::slice::from_raw_parts(data.cast::<u8>(), self.bytes);
            write(&self.path, bytes, &self.note);
            self.buffer.Unmap(0, None);
        }
    }
}

pub(crate) fn write(path: &Path, bytes: &[u8], note: &str) {
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        let zero = bytes.iter().filter(|&&b| b == 0).count();
        std::fs::write(
            path.with_extension("txt"),
            format!(
                "{note}\nbytes={} zero_bytes={zero} blake3={}\n",
                bytes.len(),
                blake3::hash(bytes)
            ),
        )
    })();
    if let Err(error) = result {
        tracing::warn!(%error, path = %path.display(), "buffer capture not saved");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn snapshots_preserve_bytes_at_each_command() {
        let Some(host) = super::host() else { return };
        let list = host.new_list().unwrap();
        let (source, data) = host.shared_buffer(256).unwrap();
        let (replacement, replacement_data) = host.shared_buffer(256).unwrap();
        // SAFETY: mapped buffers have 256 bytes each.
        unsafe {
            std::ptr::write_bytes(data, 0x35, 256);
            std::ptr::write_bytes(replacement_data, 0x79, 256);
        }
        let folder = tempfile::tempdir().unwrap();
        let before = folder.path().join("before.bin");
        let after = folder.path().join("after.bin");
        super::Snapshot::record(&list, &source, 16, 32, before.clone(), "before".into());
        // SAFETY: live COMMON-state buffers, an open list, valid ranges.
        unsafe { list.list.CopyBufferRegion(&source, 16, &replacement, 0, 32) };
        super::Snapshot::record(&list, &source, 16, 32, after.clone(), "after".into());
        assert!(
            !before.exists(),
            "readback must wait for the submission fence"
        );
        list.close();
        host.execute(&[&list]);
        assert_eq!(std::fs::read(before).unwrap(), vec![0x35; 32]);
        assert_eq!(std::fs::read(after).unwrap(), vec![0x79; 32]);
        list.reset();
    }
}
