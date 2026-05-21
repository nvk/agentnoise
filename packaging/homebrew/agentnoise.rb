class Agentnoise < Formula
  desc "Chat with local coding agents through White Noise"
  homepage "https://agentnoise.org"
  url "https://github.com/nvk/agentnoise/archive/refs/tags/v0.1.26.tar.gz"
  sha256 "0c0f2a0d158a7615d6cc006b44df7711906c08fd6c3eea53c3327afbd8716b9c"
  license "MIT"
  head "https://github.com/nvk/agentnoise.git", branch: "main"

  depends_on "rust" => :build

  resource "whitenoise-rs" do
    url "https://github.com/marmot-protocol/whitenoise-rs.git",
        revision: "917ad14f8eed5fe0df623e7621a709e07f77d785"
  end

  def install
    ENV["CARGO_NET_GIT_FETCH_WITH_CLI"] = "true"
    ENV["GIT_CONFIG_GLOBAL"] = File::NULL

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

  def caveats
    <<~EOS
      Quick start with raw Codex/Claude:
        agentnoise up --direct-agents

      To keep setup/pairing alive in the background:
        brew services start nvk/tap/agentnoise

      Current Codex CLI builds do not run reliably from macOS launchd. For
      /codex jobs on macOS, run agentnoise from a login shell or tmux:
        agentnoise up --no-daemon

      Use agentnoise up anytime as the local console. If the service is already
      running, it attaches instead of starting a second listener.

      Config:
        agentnoise config path
        agentnoise config print-template
        agentnoise doctor

      If you use bondage profiles instead of raw CLIs, omit --direct-agents and
      provide codex-agentnoise / claude-agentnoise profiles.
    EOS
  end

  test do
    assert_match "agentnoise 0.1.26", shell_output("#{bin}/agentnoise --version")
  end
end
