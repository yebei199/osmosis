# Slint Study

A minimal [Slint](https://slint.dev) Android app — a tap counter — modelled on
the build shape of the `ntrack` project. The UI is written in Slint, the logic
in Rust, and the whole thing compiles to a native `.so` loaded by a
`NativeActivity`. The APK is built reproducibly inside a Docker container so the
only host requirement is Docker.

## Layout

```
ui/app.slint       the whole UI (a counter)
src/lib.rs         android_main + run_app (the UI<->Rust glue)
src/main.rs        desktop dev entry point
android/           Gradle project: NativeActivity, manifest, resources
docker/Dockerfile  builder image (JDK, Android SDK+NDK, Rust, cargo-ndk)
scripts/build-apk.sh   cargo-ndk cross-build + gradle assembleDebug
build.sh           host entrypoint (drives Docker)
```

## Build the APK

```sh
./build.sh                 # -> dist/slint-study-debug.apk
adb install -r dist/slint-study-debug.apk
```

Build for the emulator (x86_64) or multiple ABIs:

```sh
ABIS="x86_64" ./build.sh
ABIS="arm64-v8a armeabi-v7a x86_64" ./build.sh
```

## Desktop dev build

Run the same UI on the desktop (needs `libfontconfig1-dev`):

```sh
cargo run --features desktop
```

## License

MIT
