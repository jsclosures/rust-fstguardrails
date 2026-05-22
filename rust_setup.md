# ⚡ LUME: Rust Toolchain & Environment Matrix

Welcome to the low-overhead, high-performance world of Rust. This guide is curated by **Steve Harris** ([jsclosures](https://github.com/jsclosures)) and **Kord Campbell** ([kordless](https://github.com/kordless)) to get your local environment fully configured for Lume's zero-dependency systems primitives on any operating system.

---

<div align="center">

[![Rust Version](https://img.shields.io/badge/rustc-1.74%2B-blue.svg?style=for-the-badge&logo=rust&color=FF4400&labelColor=222222)]()
[![Platform Matrix](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-blue.svg?style=for-the-badge&logo=cargo&color=00D2FF&labelColor=222222)]()
[![System Architecture](https://img.shields.io/badge/arch-x86__64%20%7C%20arm64-blue.svg?style=for-the-badge&logo=cpu&color=9F44FF&labelColor=222222)]()

</div>

---

## 🛠️ The Rustup Bootstrap: Multi-Platform Setup

We use `rustup`—the official Rust toolchain installer and version manager—to compile Lume's heavy-duty bitset math and FST match matrices.

### 🐧 1. Linux & WSL2 (The Ultimate Dev Environment)

Lume operates at maximum throughput under POSIX architectures. To install the toolchain and essential build utilities:

```bash
# Update and install system build essentials
sudo apt update && sudo apt install -y build-essential curl pkg-config libssl-dev

# Bootstrap rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```
*During installation, press `1` to proceed with the default setup.*

Once completed, reload your shell profile:
```bash
source $HOME/.cargo/env
```

### 🍎 2. macOS (Apple Silicon M1/M2/M3 & Intel)

macOS systems compile Rust seamlessly. Ensure you have the Xcode Command Line Tools installed before setting up `rustup`:

```bash
# Install Xcode Command Line Tools
xcode-select --install

# Bootstrap rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```
*Reload your shell:*
```bash
source $HOME/.cargo/env
```

### 🪟 3. Windows 11 / 10 (MSVC Native Setup)

For bare-metal Windows performance, we compile via the **MSVC toolchain** (highly recommended over GNU on Windows).

#### Option A: Interactive Installer (Default)
1. Download and run [rustup-init.exe](https://win.rustup.rs/).
2. When prompted, install the **Visual Studio Build Tools** (MSVC C++ Build Tools workload).
3. Select Option `1` (Proceed with default installation).

#### Option B: Winget CLI (Hacker Style)
Open PowerShell as Administrator and run:
```powershell
# Install Build Tools
winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# Install Rustup
winget install --id Rustlang.Rustup
```
*Restart your terminal to register the new system path environment.*

---

## 🔍 Validation Protocol

Run these sanity checks to ensure the toolchain is healthy and available:

```bash
# Print compiler and package manager versions
rustc --version
cargo --version
rustup --version
```

Expected minimum specifications:
- `rustc 1.74.0` or greater.
- Dynamic toolchain targeting active architecture.

---

## 🚀 Hyper-Speed Optimization: Turbocharging Cargo

To make Lume builds compile in under **2 seconds**, apply these systems optimizations to your local profile.

### 1. Enable Global Compilation Caching (`sccache`)
Avoid compiling dependencies twice by installing Mozilla's `sccache`:
```bash
cargo install sccache
```
Add the following to your global config (`~/.cargo/config.toml` on Unix, `%USERPROFILE%\.cargo\config.toml` on Windows):
```toml
[build]
rustc-wrapper = "sccache"
```

### 2. Swap to High-Speed Linkers
The default GNU/MSVC linkers can be slow. Swap them out for instant linking:

*   **Linux (Mold Linker)**:
    ```bash
    sudo apt install -y mold
    ```
    Add to `config.toml`:
    ```toml
    [target.x86_64-unknown-linux-gnu]
    linker = "clang"
    rustflags = ["-C", "link-arg=-fuse-ld=mold"]
    ```
*   **macOS (zld Linker)**:
    ```bash
    brew install michaeleisel/zld/zld
    ```
    Add to `config.toml`:
    ```toml
    [target.x86_64-apple-darwin]
    rustflags = ["-C", "link-arg=-fuse-ld=zld"]

    [target.aarch64-apple-darwin]
    rustflags = ["-C", "link-arg=-fuse-ld=zld"]
    ```

---

## 📚 Build and Execute Lume

Now that your toolchain is tuned, clone Lume and verify the engine:

```bash
# Verify all primitives against unit tests
cargo test

# Compile fully optimized binaries (Primitive 4, 6, & 7 ready)
cargo build --release

# Run hybrid BM25 lexical/semantic search engine
DATA="examples/data" ALPHA=2.0 cargo run --release --bin hatcher-boost -- examples/monte_cristo.md
```

---

<div align="center">
<b>L U M E // ENGINE MATRIX IMPLEMENTATION BY STEVE & KORDLESS</b>
</div>
