fn main() {
    // tauri-build отключён в MVP (требовал icons/icon.ico). Но фронтенд вшивается
    // в бинарь через generate_context! при компиляции, поэтому говорим Cargo
    // пересобирать крейт при любом изменении ui/ — иначе правки index.html не
    // попадут в сборку.
    println!("cargo:rerun-if-changed=ui");
}
