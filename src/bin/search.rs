fn main() {
    lume::cli::search::run(std::env::args().skip(1).collect());
}
