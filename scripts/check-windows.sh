#!/bin/sh
# Type-check and lint the whole workspace for Windows from macOS or Linux,
# with no Windows toolchain. Needs once: rustup target add x86_64-pc-windows-msvc
#
# What it proves: every crate, binary and test compiles for
# x86_64-pc-windows-msvc and passes clippy there. What it does not: that
# anything links or runs. The C libraries in the tree (SQLite, aws-lc) are not
# compiled: the C compiler, archiver and cmake are stand-ins that only create
# their output files, which is enough for `cargo check`, which never links.
# Only a Windows machine or the `windows` job in CI builds and runs it.
set -e
cd "$(dirname "$0")/.."
fake=$(mktemp -d)
trap 'rm -rf "$fake"' EXIT
cat > "$fake/cc" <<'EOF'
#!/bin/sh
out=""; prev=""
for a in "$@"; do
  case "$prev" in -o) out="$a";; esac
  case "$a" in -Fo*) out="${a#-Fo}";; /Fo*) out="${a#/Fo}";; esac
  prev="$a"
done
[ -n "$out" ] && : > "$out"
exit 0
EOF
cat > "$fake/ar" <<'EOF'
#!/bin/sh
for a in "$@"; do case "$a" in -out:*|/OUT:*) : > "${a#*:}";; -*) ;; *.a|*.lib) : > "$a";; esac; done
exit 0
EOF
cat > "$fake/cmake" <<'EOF'
#!/bin/sh
case "$1" in --version) echo "cmake version 3.28.0";; esac
exit 0
EOF
chmod +x "$fake/cc" "$fake/ar" "$fake/cmake"
export CC_x86_64_pc_windows_msvc="$fake/cc" CXX_x86_64_pc_windows_msvc="$fake/cc"
export AR_x86_64_pc_windows_msvc="$fake/ar" CMAKE="$fake/cmake" AWS_LC_SYS_PREBUILT_NASM=1
cargo clippy --target x86_64-pc-windows-msvc --workspace --all-targets --quiet -- -D warnings
echo "type-checked and linted for x86_64-pc-windows-msvc (not linked, not run)"
