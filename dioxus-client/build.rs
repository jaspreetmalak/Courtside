use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=ios/CallVoice.swift");
    println!("cargo:rerun-if-changed=ios/VoiceCodec.swift");
    println!("cargo:rerun-if-changed=ios/CallListen.swift");
    println!("cargo:rerun-if-changed=ios/ListenAudio.swift");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("ios") {
        return;
    }
    let target = env::var("TARGET").unwrap();
    println!("cargo:rustc-env=IPHONEOS_DEPLOYMENT_TARGET=15.0");
    let simulator = target.ends_with("-sim") || target.starts_with("x86_64");
    let sdk_name = if simulator {
        "iphonesimulator"
    } else {
        "iphoneos"
    };
    let sdk = Command::new("xcrun")
        .args(["--sdk", sdk_name, "--show-sdk-path"])
        .output()
        .expect("locate iOS SDK");
    assert!(sdk.status.success(), "iOS SDK is unavailable");
    let sdk = String::from_utf8(sdk.stdout).unwrap().trim().to_string();
    let arch = if target.starts_with("x86_64") {
        "x86_64"
    } else {
        "arm64"
    };
    let swift_target = format!(
        "{arch}-apple-ios15.0{}",
        if simulator { "-simulator" } else { "" }
    );
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let archive = out.join("libCourtsideVoice.a");
    let status = Command::new("xcrun")
        .args([
            "--sdk",
            sdk_name,
            "swiftc",
            "-swift-version",
            "5",
            "-O",
            "-parse-as-library",
            "-emit-library",
            "-static",
            "-module-name",
            "CourtsideVoice",
            "-target",
            &swift_target,
            "-sdk",
            &sdk,
        ])
        .args([
            "ios/VoiceCodec.swift",
            "ios/ListenAudio.swift",
            "ios/CallListen.swift",
            "ios/CallVoice.swift",
            "-o",
        ])
        .arg(&archive)
        .status()
        .expect("compile native CallKit audio bridge");
    assert!(status.success(), "native CallKit bridge did not compile");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=CourtsideVoice");
    // Swift Mach-O objects carry their own linker options. Supply the SDK and
    // toolchain library paths so clang can resolve those autolinked runtimes.
    let swift = Command::new("xcrun")
        .args(["--find", "swiftc"])
        .output()
        .unwrap();
    let swift = PathBuf::from(String::from_utf8(swift.stdout).unwrap().trim());
    let toolchain = swift.parent().unwrap().parent().unwrap();
    println!(
        "cargo:rustc-link-search=native={}/lib/swift/{sdk_name}",
        toolchain.display()
    );
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    for framework in ["AVFoundation", "CallKit", "Foundation"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
