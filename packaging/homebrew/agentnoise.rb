class Agentnoise < Formula
  desc "Chat with local coding agents through White Noise"
  homepage "https://agentnoise.com"
  url "https://github.com/nvk/agentnoise.git", tag: "v0.1.14"
  license "MIT"
  head "https://github.com/nvk/agentnoise.git", branch: "main"

  depends_on "rust" => :build

  resource "whitenoise-rs" do
    url "https://github.com/marmot-protocol/whitenoise-rs.git",
        revision: "917ad14f8eed5fe0df623e7621a709e07f77d785"
  end

  def install
    system "cargo", "install", *std_cargo_args

    resource("whitenoise-rs").stage do
      system "cargo", "install",
        "--path", "crates/whitenoise-cli",
        "--root", prefix,
        "--bin", "wn",
        "--bin", "wnd"
    end
  end

  service do
    run [opt_bin/"agentnoise", "up"]
    environment_variables PATH: "#{HOMEBREW_PREFIX}/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    keep_alive true
    log_path var/"log/agentnoise.log"
    error_log_path var/"log/agentnoise.err.log"
  end

  test do
    assert_match "agentnoise 0.1.14", shell_output("#{bin}/agentnoise --version")
  end
end
