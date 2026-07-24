# InkBridge BOOX integration

This fork extends Inkread with a BOOX/Onyx low-latency pen path while preserving the existing vendor-neutral Rust ink core.

The current integration branch first validates Onyx SDK compilation and APK packaging inside Inkread's canonical build pipeline. Reader input routing remains on the existing Supernote path until that build gate is green.
