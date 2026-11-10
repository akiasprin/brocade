# Brocade vt100 patch

Source: crates.io vt100 0.18.1, upstream https://github.com/doy/vt100-rust at commit `fb0f54ec7222f4805f33cd3ca530229e0a0fe6ad`. MIT license retained in LICENSE.

Local changes:

- Preserve soft-wrap boundaries when a wide character reaches an empty final column.
- Preserve a character widened by a variation selector when it wraps and scrolls. Mark the resulting margin cell so logical captures omit padding while visual captures preserve its attributes; the marker fits the existing 32-byte cell.
- Expose a read-only physical-row capture accessor. The parent terminal uses actual cells to locate cursors, including combining sequences and variation selectors, instead of estimating character widths.

Regression coverage lives in the parent terminal crate. Keep std disabled so VTE retains a fixed 1 KiB OSC buffer. An empty `[workspace]` and the parent exclusion keep upstream-only test dependencies outside the Shim's pinned graph. Original metadata and license are retained.
