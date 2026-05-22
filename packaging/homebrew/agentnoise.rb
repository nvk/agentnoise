class Agentnoise < Formula
  desc "Chat with local coding agents through Marmot / Darkmatter (v2 embedded)"
  homepage "https://agentnoise.org"
  url "https://github.com/nvk/agentnoise/archive/refs/tags/v0.2.0.tar.gz"
  # NOTE: regenerate sha256 when the v0.2.0 tag is cut. The 0.1.24 sha is
  # intentionally left here so a stray `brew install` against an unprepared
  # release fails loudly rather than silently.
  sha256 "43331ffd7432009e6147a46502958e0794bbb5655f959fd71845cf5c3c03aa99"
  license "MIT"
  head "https://github.com/nvk/agentnoise.git", branch: "main"

  depends_on "rust" => :build

  # v0.2.0 embeds the darkmatter v2 Marmot protocol crates directly — no more
  # external `wn` / `wnd` install. See docs/darkmatter.md for the migration.

  def install
    ENV["CARGO_NET_GIT_FETCH_WITH_CLI"] = "true"
    ENV["GIT_CONFIG_GLOBAL"] = File::NULL

    system "cargo", "install", *std_cargo_args
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
    assert_match "agentnoise 0.2.0", shell_output("#{bin}/agentnoise --version")
  end
end
