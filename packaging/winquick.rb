# Homebrew formula for WinQuick.
#
# Tap layout:  carlbomsdata/homebrew-tap/Formula/winquick.rb
# Install:     brew install carlbomsdata/tap/winquick
#
# The sha256 below is filled in from dist/*.sha256 after `scripts/release.sh`.
class Winquick < Formula
  desc "Run commands inside a real, disposable Windows environment"
  homepage "https://github.com/carlbomsdata/winquick"
  url "https://github.com/carlbomsdata/winquick/releases/download/v0.4.3/winquick-0.4.3-darwin-arm64.tar.gz"
  sha256 "b8e9ef82f201bf3c2bd2b53b116698aa400df9bb368739fceac82d1838afe35c"
  license "Apache-2.0"

  # Apple Silicon only: the guest is ARM64 Windows and acceleration comes from
  # Apple's Hypervisor Framework.
  depends_on arch: :arm64
  # Used by `winquick setup` to set one value in a Windows registry hive.
  depends_on "hivex"
  depends_on :macos
  # Runs the Windows guest. A separate process, never linked into winquick.
  depends_on "qemu"

  def install
    bin.install "bin/winquick"
    (libexec/"winquick").install Dir["libexec/winquick/*"]
    doc.install Dir["share/doc/winquick/*"]
    # `winquick capability install desktop` builds its guest bridge from these,
    # inside Windows, at install time.
    (pkgshare/"wqui").install Dir["share/winquick/wqui/*"]
  end

  def caveats
    <<~EOS
      WinQuick needs Microsoft's Windows validation runtime, which Microsoft
      distributes under its own licence. Set it up with:

        winquick setup --accept-microsoft-terms

      or point it at an image you already have:

        winquick setup --from ~/Downloads/<validation-os>.iso

      Then try:

        winquick run -- cmd /c ver
    EOS
  end

  test do
    assert_match "winquick #{version}", shell_output("#{bin}/winquick --version")
    # No runtime is installed in the test sandbox, so this should fail cleanly
    # with an actionable message rather than crashing.
    output = shell_output("#{bin}/winquick run -- cmd /c ver 2>&1", 1)
    assert_match "winquick setup", output
  end
end
