use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico_path = manifest_dir
        .join("..")
        .join("..")
        .join("assets")
        .join("ri_icon.ico");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", ico_path.display());

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    if !ico_path.exists() {
        println!(
            "cargo:warning=Missing Windows application icon at {}. Run `cargo run -p ide-app -- --export-icon assets/ri_icon.ico` to regenerate it.",
            ico_path.display()
        );
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if target_env == "gnu" {
        let rc_path = out_dir.join("app_icon.rc");
        let obj_path = out_dir.join("app_icon.o");
        let ico_rc = ico_path.to_string_lossy().replace('\\', "/");
        fs::write(&rc_path, format!("1 ICON \"{}\"\n", ico_rc)).expect("write app_icon.rc");

        for tool in ["x86_64-w64-mingw32-windres", "windres"] {
            if let Ok(status) = Command::new(tool)
                .arg("-i")
                .arg(&rc_path)
                .arg("-o")
                .arg(&obj_path)
                .status()
            {
                if status.success() {
                    println!("cargo:rustc-link-arg-bins={}", obj_path.display());
                    return;
                }
            }
        }
        panic!("Failed to find windres to compile Windows icon resource for GNU target");
    }

    let res_path = out_dir.join("app_icon.res");
    if compile_with_rc(&ico_path, &out_dir, &res_path) {
        println!("cargo:rustc-link-arg-bins={}", res_path.display());
        return;
    }

    // Pure-Rust fallback when rc.exe / llvm-rc is unavailable in the build environment.
    write_ico_res_file(&ico_path, &res_path).expect("write fallback app_icon.res");
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
}

fn compile_with_rc(ico_path: &Path, out_dir: &Path, res_path: &Path) -> bool {
    let rc_path = out_dir.join("app_icon.rc");
    let ico_rc = ico_path.to_string_lossy().replace('\\', "/");
    let rc_body = format!("1 ICON \"{}\"\n", ico_rc);
    if fs::write(&rc_path, rc_body).is_err() {
        return false;
    }

    if let Some(rc_exe) = find_rc_exe() {
        let ok = Command::new(&rc_exe)
            .arg("/nologo")
            .arg("/fo")
            .arg(res_path)
            .arg(&rc_path)
            .status()
            .is_ok_and(|s| s.success());
        if ok && res_path.exists() {
            return true;
        }
    }

    false
}

fn find_rc_exe() -> Option<PathBuf> {
    for candidate in ["rc.exe", "llvm-rc.exe"] {
        if Command::new(candidate)
            .arg("/?")
            .output()
            .is_ok()
        {
            return Some(PathBuf::from(candidate));
        }
    }

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => "x64",
    };

    for root_var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(root) = env::var(root_var) {
            let kit_bin = PathBuf::from(root).join("Windows Kits").join("10").join("bin");
            if let Ok(entries) = fs::read_dir(&kit_bin) {
                let mut versions: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_dir())
                    .collect();
                versions.sort();
                versions.reverse();
                for ver_dir in versions {
                    let rc = ver_dir.join(arch).join("rc.exe");
                    if rc.is_file() {
                        return Some(rc);
                    }
                }
            }
        }
    }

    None
}

fn write_ico_res_file(ico_path: &Path, res_path: &Path) -> std::io::Result<()> {
    let ico = fs::read(ico_path)?;
    if ico.len() < 6 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ICO file too small",
        ));
    }
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    if ico.len() < 6 + count * 16 || count == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid ICO directory",
        ));
    }

    let mut out = Vec::new();
    // Standard empty RES header entry (32 bytes)
    push_res_entry(&mut out, 0, 0, 0, 0, &[]);

    // GRPICONDIR (6 bytes) + N * GRPICONDIRENTRY (14 bytes)
    let mut group_data = Vec::with_capacity(6 + count * 14);
    group_data.extend_from_slice(&ico[0..6]);

    for idx in 0..count {
        let entry_off = 6 + idx * 16;
        let entry = &ico[entry_off..entry_off + 16];
        let bytes_in_res = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let image_offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        let image_end = image_offset.saturating_add(bytes_in_res);
        if image_end > ico.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ICO frame out of bounds",
            ));
        }

        let icon_id = (idx + 1) as u16;
        // RT_ICON = 3
        push_res_entry(&mut out, 3, icon_id, 0x1010, 0x0409, &ico[image_offset..image_end]);

        // First 12 bytes of ICONDIRENTRY match GRPICONDIRENTRY, followed by u16 icon_id
        group_data.extend_from_slice(&entry[0..12]);
        group_data.extend_from_slice(&icon_id.to_le_bytes());
    }

    // RT_GROUP_ICON = 14, resource ID = 1
    push_res_entry(&mut out, 14, 1, 0x1030, 0x0409, &group_data);

    fs::write(res_path, out)
}

fn push_res_entry(
    out: &mut Vec<u8>,
    res_type: u16,
    res_id: u16,
    memory_flags: u16,
    language_id: u16,
    data: &[u8],
) {
    // RESOURCEHEADER with numeric type and numeric name (32 bytes total)
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    out.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&res_type.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&res_id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    out.extend_from_slice(&memory_flags.to_le_bytes());
    out.extend_from_slice(&language_id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
