fn main() {
    println!("cargo:rerun-if-changed=app_icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=locales");

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winres::WindowsResource::new();
        res.set_icon("app_icon.ico");
        res.set("ProductName", "MakerCalc");
        res.set("FileDescription", "MakerCalc");
        if res.compile().is_ok() {
            // MinGW ld discards resources in static archives unless the .o is linked directly.
            if std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default() == "gnu"
                && let Ok(out_dir) = std::env::var("OUT_DIR")
            {
                let out_path = std::path::Path::new(&out_dir);

                // WinLibs MinGW GCC links default-manifest.o, conflicting with gpui-pre's manifest.
                // The ".rsrc merge failure: multiple non-default manifests" drops all resources, including the icon.
                // Custom GCC specs omit default-manifest.o to prevent the conflict.
                let spec_file = out_path.join("no-default-manifest.specs");
                let spec_content = "*endfile:\n%{mdaz-ftz:crtfastmath.o%s;Ofast|ffast-math|funsafe-math-optimizations:%{!shared:%{!mno-daz-ftz:crtfastmath.o%s}}}   %{fvtable-verify=none:%s;     fvtable-verify=preinit:vtv_end.o%s;     fvtable-verify=std:vtv_end.o%s}   crtend.o%s\n";
                let _ = std::fs::write(&spec_file, spec_content);

                let spec_arg = format!("{}/no-default-manifest.specs", out_dir.replace('\\', "/"));
                println!("cargo:rustc-link-arg=-specs={spec_arg}");

                let obj = format!("{}/resource.o", out_dir.replace('\\', "/"));
                if std::path::Path::new(&obj).exists() {
                    println!("cargo:rustc-link-arg={obj}");
                }
            }
        }
    }
}
