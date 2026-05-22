fn main() {
    lume::cli::hatcher_boost::run(std::env::args().skip(1).collect());
}
