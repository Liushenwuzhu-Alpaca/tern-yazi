import subprocess

from tern_yazi import launcher


def test_bin_info_missing_binary(monkeypatch):
    monkeypatch.setattr(launcher.shutil, "which", lambda name: None)
    assert launcher.bin_info("yazi") == (None, None)


def test_bin_info_reads_first_version_line(monkeypatch, tmp_path):
    binary = tmp_path / "yazi"
    binary.write_text("")
    monkeypatch.setattr(launcher.shutil, "which", lambda name: str(binary))

    def fake_run(cmd, **kwargs):
        assert cmd == [str(binary), "--version"]
        return subprocess.CompletedProcess(
            cmd, 0, stdout="yazi 0.4.2 (abc123 2026-01-01)\nextra\n", stderr=""
        )

    monkeypatch.setattr(launcher.subprocess, "run", fake_run)
    assert launcher.bin_info("yazi") == (str(binary), "yazi 0.4.2 (abc123 2026-01-01)")


def test_unknown_args_rejected(capsys):
    assert launcher.main(["--bogus"]) == 2
    assert "unknown arguments" in capsys.readouterr().err
