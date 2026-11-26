class Webdelta < Formula
  desc "Single-binary website and API change monitor with HTTP/CDP rendering and MCP server"
  homepage "https://github.com/luminariadev/webdelta"
  version "0.3.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/luminariadev/webdelta/releases/download/v0.3.0/webdelta-v0.3.0-aarch64-apple-darwin.tar.gz"
      sha256 "384a9fe059382762019c53cb9230524f955187d9cbbbab856614cc279c60c62b"
    else
      url "https://github.com/luminariadev/webdelta/releases/download/v0.3.0/webdelta-v0.3.0-x86_64-apple-darwin.tar.gz"
      sha256 "0b562563d54372997db05909311f6fb8465800cd308623f7e7194a1de504583c"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/luminariadev/webdelta/releases/download/v0.3.0/webdelta-v0.3.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "bb3acf0288c5ea5c4bef118c1457c99c331e1c2f79a220c69e043a903ae92c75"
    end
  end

  def install
    bin.install "webdelta"
  end

  test do
    assert_match "webdelta #{version}", shell_output("#{bin}/webdelta --version")
  end
end
