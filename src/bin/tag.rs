fn main() {
    lume::cli::tag::run(std::env::args().skip(1).collect());
}
