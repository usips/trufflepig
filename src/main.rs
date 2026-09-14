fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = trufflepig::cli::emission::execute(
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::exit(code);
}
