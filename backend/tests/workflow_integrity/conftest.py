import pytest


@pytest.fixture(autouse=True)
def no_real_rekordbox(monkeypatch, tmp_path):
    """Analysis saves read rekordbox; never let them open the developer's library."""
    monkeypatch.setenv("PLUMDECK_REKORDBOX_DB", str(tmp_path / "no-rekordbox" / "master.db"))
