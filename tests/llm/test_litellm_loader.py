import builtins
import sys
from types import SimpleNamespace

import aish.llm.loader as litellm_loader


def _reset_loader_state():
    litellm_loader._cached_litellm = litellm_loader._SENTINEL
    litellm_loader._preload_thread = None


def test_load_litellm_sanitizes_proxy_env_before_import(monkeypatch):
    _reset_loader_state()
    monkeypatch.delenv("HTTPS_PROXY", raising=False)
    monkeypatch.setenv("HTTP_PROXY", " http://proxy.example:8080\nignored ")
    monkeypatch.setenv("ALL_PROXY", "socks5://proxy.example:1080\rignored")
    monkeypatch.delitem(sys.modules, "litellm", raising=False)

    fake_module = SimpleNamespace()
    real_import = builtins.__import__

    def fake_import(name, globals=None, locals=None, fromlist=(), level=0):
        if name == "litellm":
            assert sys.modules.get("litellm") is None
            assert litellm_loader.os.environ["HTTP_PROXY"] == "http://proxy.example:8080"
            assert litellm_loader.os.environ["ALL_PROXY"] == "socks5://proxy.example:1080"
            return fake_module
        return real_import(name, globals, locals, fromlist, level)

    monkeypatch.setattr(builtins, "__import__", fake_import)

    module = litellm_loader.load_litellm()

    assert module is fake_module


def test_load_litellm_preserves_clean_proxy_env(monkeypatch):
    _reset_loader_state()
    monkeypatch.setenv("HTTPS_PROXY", "https://proxy.example:8443")
    monkeypatch.setenv("NO_PROXY", "localhost,127.0.0.1")
    monkeypatch.delitem(sys.modules, "litellm", raising=False)

    fake_module = SimpleNamespace()
    real_import = builtins.__import__

    def fake_import(name, globals=None, locals=None, fromlist=(), level=0):
        if name == "litellm":
            assert litellm_loader.os.environ["HTTPS_PROXY"] == "https://proxy.example:8443"
            assert litellm_loader.os.environ["NO_PROXY"] == "localhost,127.0.0.1"
            return fake_module
        return real_import(name, globals, locals, fromlist, level)

    monkeypatch.setattr(builtins, "__import__", fake_import)

    module = litellm_loader.load_litellm()

    assert module is fake_module