fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = odk_sneakernet::run(&args, &mut std::io::stdout());
    std::process::exit(code);
}
