import pytest
import ridal


def test_module_metadata_exists():
    assert isinstance(ridal.__version__, str)
    assert isinstance(ridal.all_steps, list)
    assert isinstance(ridal.all_step_descriptions, dict)
    assert isinstance(ridal.all_formats, list)
    assert isinstance(ridal.all_format_descriptions, dict)


def test_known_format_names_exist():
    assert "ramac" in ridal.all_formats
    assert "pulseekko" in ridal.all_formats
    assert "ramac" in ridal.all_format_descriptions
    assert "pulseekko" in ridal.all_format_descriptions


def test_run_cli_raises_migration_error():
    with pytest.raises(NotImplementedError):
        ridal.run_cli("--default", "line01.rad")
