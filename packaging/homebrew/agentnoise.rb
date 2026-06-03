class Agentnoise < Formula
  desc "Chat with local coding agents through White Noise"
  homepage "https://agentnoise.org"
  url "https://github.com/nvk/agentnoise/archive/refs/tags/v0.1.35.tar.gz"
  sha256 "46c3bc75ecb6f2095da043012b59f0ca03b47cd149c51a97e78fea94c5896a8b"
  license "MIT"
  head "https://github.com/nvk/agentnoise.git", branch: "main"

  depends_on "rust" => :build
  depends_on "sqlite"

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
    run [opt_bin/"agentnoise", "transport", "run"]
    environment_variables PATH: "#{HOMEBREW_PREFIX}/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    keep_alive true
    log_path var/"log/agentnoise.log"
    error_log_path var/"log/agentnoise.err.log"
  end

  def caveats
    <<~EOS
      Quick start without agentbondage, using raw Codex/Claude:
        agentnoise up --direct-agents --no-listen
        brew services start nvk/tap/agentnoise
        agentnoise worker start

      Or run foreground setup/listening from a terminal:
        agentnoise up --direct-agents

      Homebrew keeps the White Noise transport alive. Local Codex/Claude/Hermes
      jobs run from your login shell worker:
        agentnoise worker start
      If tmux is installed, detach it with:
        agentnoise worker start --tmux

      Use agentnoise up anytime as the local console. If the service is already
      running, it attaches instead of starting a second listener.

      Config:
        agentnoise config path
        agentnoise config launcher direct
        agentnoise config print-template
        agentnoise doctor

      If you use bondage profiles instead of raw CLIs, omit --direct-agents and
      provide codex-agentnoise / claude-agentnoise profiles.
    EOS
  end

  test do
    assert_match "agentnoise 0.1.35", shell_output("#{bin}/agentnoise --version")
  end
end
