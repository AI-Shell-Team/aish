"""
Tests for path autocompletion with proper quoting.
"""

import os
import tempfile
from unittest.mock import MagicMock, Mock

import pytest
from prompt_toolkit.document import Document

from aish.shell import QuotedPathCompleter


class TestQuotedPathCompleter:
    """Test the custom path completer that quotes paths with spaces."""

    def setup_method(self):
        """Set up test fixtures."""
        self.temp_dir = tempfile.mkdtemp()
        self.original_cwd = os.getcwd()
        os.chdir(self.temp_dir)

        # Create test directories and files with spaces and special characters
        test_paths = [
            "normal_dir",
            "dir with spaces",
            "dir[with]brackets",
            "dir(with)parens",
            "dir'with'quotes",
            'dir"with"doublequotes',
            "file.txt",
            "file with spaces.txt",
            "file[with]brackets.txt",
        ]

        for path in test_paths:
            if path.endswith(".txt"):
                # Create file
                with open(path, "w") as f:
                    f.write("test")
            else:
                # Create directory
                os.makedirs(path, exist_ok=True)

        self.completer = QuotedPathCompleter(expanduser=True)

    def teardown_method(self):
        """Clean up after tests."""
        os.chdir(self.original_cwd)
        import shutil

        shutil.rmtree(self.temp_dir)

    def test_completes_normal_paths_without_quotes(self):
        """Test that normal paths without spaces are not quoted."""
        document = Document("normal_d")
        complete_event = Mock()

        completions = list(self.completer.get_completions(document, complete_event))

        # Debug: Print actual completions
        print(f"Normal path completions: {[c.text for c in completions]}")

        # Should find completion without quotes
        assert len(completions) >= 1
        assert any(
            "ir" in completion.text
            and "'" not in completion.text
            and '"' not in completion.text
            for completion in completions
        )

    def test_completes_paths_with_spaces_with_quotes(self):
        """Test that paths with spaces are automatically quoted."""
        document = Document("dir with")
        complete_event = Mock()

        completions = list(self.completer.get_completions(document, complete_event))

        # Debug: Print actual completions
        print(f"Spaces path completions: {[c.text for c in completions]}")

        # Should find completion with quotes - new implementation returns full path
        assert len(completions) >= 1
        assert any(
            "dir with spaces" in completion.text and "'" in completion.text
            for completion in completions
        )

    def test_completes_paths_with_brackets_with_quotes(self):
        """Test that paths with brackets are automatically quoted."""
        document = Document("dir[with")
        complete_event = Mock()

        completions = list(self.completer.get_completions(document, complete_event))

        # Debug: Print actual completions
        print(f"Brackets path completions: {[c.text for c in completions]}")

        # Should find completion with quotes - new implementation returns full path
        assert len(completions) >= 1
        assert any(
            "dir[with]brackets" in completion.text and "'" in completion.text
            for completion in completions
        )

    def test_completes_paths_with_parens_with_quotes(self):
        """Test that paths with parentheses are automatically quoted."""
        document = Document("dir(with")
        complete_event = Mock()

        completions = list(self.completer.get_completions(document, complete_event))

        # Debug: Print actual completions
        print(f"Parens path completions: {[c.text for c in completions]}")

        # Should find completion with quotes - new implementation returns full path
        assert len(completions) >= 1
        assert any(
            "dir(with)parens" in completion.text and "'" in completion.text
            for completion in completions
        )

    def test_completes_files_with_spaces_with_quotes(self):
        """Test that files with spaces are automatically quoted."""
        document = Document("file with")
        complete_event = Mock()

        completions = list(self.completer.get_completions(document, complete_event))

        # Debug: Print actual completions
        print(f"File spaces completions: {[c.text for c in completions]}")

        # Should find completion with quotes - new implementation returns full path
        assert len(completions) >= 1
        assert any(
            "file with spaces.txt" in completion.text and "'" in completion.text
            for completion in completions
        )

    def test_completes_normal_files_without_quotes(self):
        """Test that normal files without spaces are not quoted."""
        document = Document("file.t")
        complete_event = Mock()

        completions = list(self.completer.get_completions(document, complete_event))

        # Should find completion without quotes
        assert len(completions) >= 1
        assert any(
            "xt" in completion.text
            and "'" not in completion.text
            and '"' not in completion.text
            for completion in completions
        )

    def test_preserves_display_metadata(self):
        """Test that display metadata from the base completer is preserved."""
        document = Document("normal_d")
        complete_event = Mock()

        completions = list(self.completer.get_completions(document, complete_event))

        # Should have at least one completion
        assert len(completions) >= 1

        # Check that we have proper completion objects
        for completion in completions:
            # Each completion should have text and start_position
            assert hasattr(completion, "text")
            assert hasattr(completion, "start_position")


if __name__ == "__main__":
    pytest.main([__file__])
