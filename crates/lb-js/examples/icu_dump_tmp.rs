fn main() {
    let out = std::env::args().nth(1).unwrap();
    let archive = lb_js::icu_data::find_archive(&std::env::current_exe().unwrap()).unwrap();
    let ar = std::fs::read(&archive).unwrap();
    std::fs::write(out, lb_js::icu_data::extract_from_archive(&ar).unwrap()).unwrap();
}
