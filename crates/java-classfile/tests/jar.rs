//! Exercises the archive reader and class decoder against the real JDK 8
//! `rt.jar` when one is installed, plus hand-built malformed input.

use java_classfile::{class, signature::SigType, Jar};
use std::path::PathBuf;
use std::process::Command;

/// Locates a JDK 8 `rt.jar`. Honours `FERRISJDT_TEST_RT_JAR`, then asks
/// `/usr/libexec/java_home` on macOS, then tries common Linux layouts.
fn rt_jar() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FERRISJDT_TEST_RT_JAR") {
        let path = PathBuf::from(path);
        return path.exists().then_some(path);
    }
    if let Ok(out) = Command::new("/usr/libexec/java_home")
        .args(["-v", "1.8"])
        .output()
    {
        if out.status.success() {
            let home = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            let jar = home.join("jre/lib/rt.jar");
            if jar.exists() {
                return Some(jar);
            }
        }
    }
    ["/usr/lib/jvm/java-8-openjdk-amd64/jre/lib/rt.jar"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

#[test]
fn reads_rt_jar_directory_and_decodes_arraylist() {
    let Some(path) = rt_jar() else {
        eprintln!("skipping: no JDK 8 rt.jar found");
        return;
    };
    let jar = Jar::open(&path).expect("rt.jar should open");
    // rt.jar holds roughly 20k entries; any parse that loses the directory
    // would come in far below this.
    assert!(jar.len() > 15_000, "only found {} entries", jar.len());
    assert!(jar.contains("java/util/ArrayList.class"));
    assert!(jar.contains("java/lang/String.class"));

    let bytes = jar
        .read("java/util/ArrayList.class")
        .unwrap()
        .expect("ArrayList.class present");
    let info = class::parse(&bytes).expect("ArrayList decodes");
    assert_eq!(info.internal_name, "java/util/ArrayList");
    assert_eq!(info.major_version, 52, "JDK 8 class files are major 52");

    // The generic signature must survive: ArrayList<E> extends AbstractList<E>.
    assert_eq!(info.type_params.len(), 1);
    assert_eq!(info.type_params[0].name, "E");
    let SigType::Class { name, args, .. } = info.superclass.as_ref().unwrap() else {
        panic!("superclass should be a class type")
    };
    assert_eq!(name, "java/util/AbstractList");
    assert_eq!(args.len(), 1, "superclass keeps its type argument");
    assert!(info
        .interfaces
        .iter()
        .any(|i| matches!(i, SigType::Class { name, .. } if name == "java/util/List")));

    // add(E) returns boolean and takes the class type parameter, not Object:
    // this is what makes `list.add` complete with the element type.
    let add = info
        .methods
        .iter()
        .find(|m| m.name == "add" && m.params.len() == 1)
        .expect("add(E) present");
    assert_eq!(add.params[0], SigType::Var("E".into()));
    assert_eq!(add.ret, SigType::Primitive('Z'));

    let get = info.methods.iter().find(|m| m.name == "get").unwrap();
    assert_eq!(get.params[0], SigType::Primitive('I'));
    assert_eq!(get.ret, SigType::Var("E".into()));

    assert!(info.fields.iter().any(|f| f.name == "size"));
}

#[test]
fn decodes_interfaces_enums_and_nested_classes() {
    let Some(path) = rt_jar() else { return };
    let jar = Jar::open(&path).unwrap();

    let comparable =
        class::parse(&jar.read("java/lang/Comparable.class").unwrap().unwrap()).unwrap();
    assert!(comparable.is_interface());
    assert_eq!(comparable.type_params.len(), 1);
    let compare_to = comparable
        .methods
        .iter()
        .find(|m| m.name == "compareTo")
        .unwrap();
    assert_eq!(compare_to.params[0], SigType::Var("T".into()));

    let entry = class::parse(&jar.read("java/util/Map$Entry.class").unwrap().unwrap()).unwrap();
    assert!(entry.is_interface());
    assert_eq!(entry.internal_name, "java/util/Map$Entry");
    assert!(entry.methods.iter().any(|m| m.name == "getKey"));

    let map = class::parse(&jar.read("java/util/Map.class").unwrap().unwrap()).unwrap();
    assert!(
        map.inner_classes
            .iter()
            .any(|i| i.inner == "java/util/Map$Entry"),
        "Map should list Entry among its inner classes"
    );

    let day = class::parse(&jar.read("java/time/DayOfWeek.class").unwrap().unwrap()).unwrap();
    assert!(day.is_enum());
}

#[test]
fn decodes_every_class_in_java_util_and_java_lang() {
    let Some(path) = rt_jar() else { return };
    let jar = Jar::open(&path).unwrap();
    let targets: Vec<String> = jar
        .entry_names()
        .filter(|n| {
            n.ends_with(".class") && (n.starts_with("java/util/") || n.starts_with("java/lang/"))
        })
        .map(str::to_string)
        .collect();
    assert!(
        targets.len() > 1000,
        "expected a large sample, got {}",
        targets.len()
    );
    let mut failures = Vec::new();
    for name in &targets {
        let bytes = match jar.read(name) {
            Ok(Some(b)) => b,
            other => {
                failures.push(format!("{name}: read failed: {other:?}"));
                continue;
            }
        };
        if let Err(err) = class::parse(&bytes) {
            failures.push(format!("{name}: {err}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} classes failed to decode:\n{}",
        failures.len(),
        targets.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn malformed_input_errors_instead_of_panicking() {
    // Not an archive, truncated archive, and garbage bytes.
    let dir = std::env::temp_dir().join("ferrisjdt-jar-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let cases: [(&str, Vec<u8>); 4] = [
        ("empty.jar", Vec::new()),
        ("garbage.jar", vec![0u8; 512]),
        ("sig-only.jar", b"PK\x05\x06".to_vec()),
        ("truncated.jar", {
            let mut v = vec![0u8; 64];
            v.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
            v.extend_from_slice(&[0xff; 18]);
            v
        }),
    ];
    for (name, bytes) in cases {
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        let _ = Jar::open(&path); // must return, not panic
    }

    // Class files: bad magic, truncated after magic, and random bytes.
    assert!(class::parse(&[]).is_err());
    assert!(class::parse(&[0xde, 0xad, 0xbe, 0xef]).is_err());
    assert!(class::parse(&[0xca, 0xfe, 0xba, 0xbe]).is_err());
    let mut truncated = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52];
    truncated.extend_from_slice(&[0xff, 0xff]); // constant_pool_count = 65535
    assert!(class::parse(&truncated).is_err());
}

#[test]
fn truncating_a_real_class_file_never_panics() {
    let Some(path) = rt_jar() else { return };
    let jar = Jar::open(&path).unwrap();
    let bytes = jar.read("java/util/HashMap.class").unwrap().unwrap();
    // Every prefix of a valid class file is invalid input; none may panic.
    let mut step = 1;
    let mut at = 0;
    while at < bytes.len() {
        let _ = class::parse(&bytes[..at]);
        at += step;
        step = (step * 2).min(997);
    }
    // Flipping bytes exercises the constant-pool and attribute paths.
    for i in (0..bytes.len()).step_by(1021) {
        let mut corrupted = bytes.clone();
        corrupted[i] ^= 0xff;
        let _ = class::parse(&corrupted);
    }
}
