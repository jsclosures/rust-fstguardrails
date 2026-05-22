fn main() {
    lume::cli::tag_server::run(std::env::args().skip(1).collect());
}
