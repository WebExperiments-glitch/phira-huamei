use std::fs;

const XCCONFIG_PATH: &str = "xcode/Shared.xcconfig";
const XCCONFIG_VERSION_KEY: &str = "MARKETING_VERSION";

fn main() {
    let cargo_version = std::env::var("CARGO_PKG_VERSION").unwrap();

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let xcconfig_path = std::path::Path::new(&manifest_dir).parent().unwrap().join(XCCONFIG_PATH);
    let content = fs::read_to_string(&xcconfig_path).unwrap_or_else(|_| panic!("`{XCCONFIG_PATH}` not found"));

    let xcode_version = content
        .lines()
        .find(|l| l.trim_start().starts_with(XCCONFIG_VERSION_KEY))
        .and_then(|l| l.split_once('=').map(|x| x.1))
        .map(|v| v.trim())
        .unwrap_or_else(|| panic!("{XCCONFIG_VERSION_KEY} not found in {}", xcconfig_path.display()));
      /* 只比「主版本」，忽略预发布后缀。
         原因：Cargo.toml 写 0.8.2-beta.1（设置页要显示 beta），而
         xcode/Shared.xcconfig 的 MARKETING_VERSION 必须是纯数字 0.8.2
         —— iOS 的 CFBundleShortVersionString 不接受 -beta，会被拒装。
         主版本一致就通过；后缀不同是故意的，不是配置漂移。2026-11-14。 */
      fn base_version(v: &str) -> &str {
          v.split('-').next().unwrap_or(v).trim()
      }
      if base_version(&cargo_version) != base_version(xcode_version) {
          panic!(
              "Inconsistent Version:
               Cargo.toml={cargo_version}, {XCCONFIG_PATH}={xcode_version}
               （只允许 -beta.x 这类预发布后缀不同；主版本必须一致）
"
          );
      }

    println!("cargo:rerun-if-changed={}", xcconfig_path.display());
    println!("cargo:rerun-if-changed=Cargo.toml");
}
