# Third-Party Notices

## yazi

The homework submission picker in `src/tui/picker/` follows the default keymap and behavior of [yazi](https://github.com/sxyazi/yazi), and the following parts are ported from its source at commit `b4f0d123952e52c4dafe874178de2b34c82f1ab2`:

| This project | yazi source |
| --- | --- |
| `src/tui/picker/natsort.rs` | `yazi-shared/src/natsort.rs` (itself a port of Martin Pool's `strnatcmp.c`) |
| `src/tui/picker/scroll.rs` | `yazi-widgets/src/step.rs`, `yazi-widgets/src/scrollable.rs`, `squeeze_offset` in `yazi-core/src/tab/folder.rs` |
| `src/tui/picker/editor.rs` (character classes and word motions) | `yazi-widgets/src/input/chars.rs`, `gait.rs`, `actor/backward.rs`, `actor/forward.rs`, `actor/kill.rs` |
| `Backstack` in `src/tui/picker/mod.rs` | `yazi-core/src/tab/backstack.rs` |

yazi is distributed under the MIT License:

```text
MIT License

Copyright (c) 2023 - sxyazi

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
