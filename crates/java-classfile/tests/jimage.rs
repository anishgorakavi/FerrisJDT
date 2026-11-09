//! The jimage reader: malformed input is an error, and a real JDK's classes decode.

use java_classfile::{class, JImage};
use std::path::PathBuf;

fn jdk_modules() -> Option<PathBuf> {
    let home = std::env::var_os("JAVA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::process::Command::new("/usr/libexec/java_home")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|text| PathBuf::from(text.trim()))
        })?;
    let modules = home.join("lib/modules");
    modules.is_file().then_some(modules)
}

#[test]
fn rejects_files_that_are_not_jimages() {
    let dir = std::env::temp_dir().join("ferrisjdt-jimage-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("garbage");
    std::fs::write(&path, vec![0x41u8; 4096]).unwrap();
    assert!(JImage::open(&path).is_err());
    let short = dir.join("short");
    std::fs::write(&short, [0xDA, 0xDA, 0xFE, 0xCA]).unwrap();
    assert!(JImage::open(&short).is_err());
}

#[test]
fn rejects_an_absurd_index_size() {
    let dir = std::env::temp_dir().join("ferrisjdt-jimage-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let mut header = vec![0u8; 28];
    header[0..4].copy_from_slice(&0xCAFE_DADAu32.to_le_bytes());
    header[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    let path = dir.join("huge");
    std::fs::write(&path, header).unwrap();
    assert!(JImage::open(&path).is_err());
}

#[test]
fn reads_real_jdk_classes_by_name() {
    let Some(modules) = jdk_modules() else {
        eprintln!("skipping: no JDK lib/modules found");
        return;
    };
    let image = JImage::open(&modules).expect("a JDK's modules file opens");
    let mut classes = 0usize;
    image.for_each_class(|_, _| classes += 1);
    assert!(classes > 5000, "only {classes} classes listed");
    let bytes = image
        .read_class("java.base", "java/util/List")
        .expect("read succeeds")
        .expect("java.util.List exists");
    let info = class::parse(&bytes).expect("a valid class file");
    assert_eq!(info.internal_name, "java/util/List");
    assert!(image
        .read_class("java.base", "java/util/NoSuchClass")
        .unwrap()
        .is_none());
    assert!(image
        .read_class("no.such.module", "java/util/List")
        .unwrap()
        .is_none());
}
