use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let res_dir = Path::new(&manifest_dir).join("composer-spdx-licenses/res");

    let licenses_path = res_dir.join("spdx-licenses.json");
    let exceptions_path = res_dir.join("spdx-exceptions.json");

    println!("cargo:rerun-if-changed={}", licenses_path.display());
    println!("cargo:rerun-if-changed={}", exceptions_path.display());

    let licenses_json: BTreeMap<String, Value> =
        serde_json::from_str(&fs::read_to_string(&licenses_path).unwrap()).unwrap();

    let exceptions_json: BTreeMap<String, Value> =
        serde_json::from_str(&fs::read_to_string(&exceptions_path).unwrap()).unwrap();

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("spdx_data.rs");

    let mut code = String::new();

    // Generate licenses array
    code.push_str("const LICENSES: &[(&str, &str, &str, bool, bool)] = &[\n");
    for (id, val) in &licenses_json {
        let arr = val.as_array().unwrap();
        let full_name = arr[0].as_str().unwrap();
        let osi_approved = arr[1].as_bool().unwrap();
        let deprecated = arr[2].as_bool().unwrap();
        let lower = id.to_lowercase();
        code.push_str(&format!(
            "    ({:?}, {:?}, {:?}, {}, {}),\n",
            lower, id, full_name, osi_approved, deprecated
        ));
    }
    code.push_str("];\n\n");

    // Generate exceptions array
    code.push_str("const EXCEPTIONS: &[(&str, &str, &str)] = &[\n");
    for (id, val) in &exceptions_json {
        let arr = val.as_array().unwrap();
        let full_name = arr[0].as_str().unwrap();
        let lower = id.to_lowercase();
        code.push_str(&format!("    ({:?}, {:?}, {:?}),\n", lower, id, full_name));
    }
    code.push_str("];\n");

    fs::write(out_path, code).unwrap();
}
