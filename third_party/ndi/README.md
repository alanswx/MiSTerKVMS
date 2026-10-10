# NDI SDK headers

The C headers from the NDI SDK (version in `Version.txt`). Each header file carries its own MIT license notice, which permits including them here. They are used only to generate `core/src/ndi/sys.rs` (`scripts/gen-ndi-bindings.sh`).

The NDI runtime library itself (`libndi`) is **not** included. It's loaded at runtime; see `core/src/ndi/mod.rs`. NDI® is a registered trademark of Vizrt NDI AB. More information and tools are at <https://ndi.video>.
