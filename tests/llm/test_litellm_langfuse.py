#!/usr/bin/env python3
"""
测试 LiteLLM + Langfuse 集成的脚本
"""

import asyncio
import os
import sys
from pathlib import Path

import pytest

# 添加项目路径
project_root = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(project_root / "src"))


def _missing_env_vars(required_vars: list[str]) -> list[str]:
    return [var for var in required_vars if not os.getenv(var)]


def _run_litellm_langfuse_integration() -> bool:
    """Run LiteLLM + Langfuse integration test and return success status."""
    print("🧪 测试 LiteLLM + Langfuse 集成")
    print("=" * 50)

    try:
        # 导入并设置 LiteLLM
        import litellm

        # 强制设置回调
        litellm.success_callback = ["langfuse"]
        litellm.failure_callback = ["langfuse"]

        print("✅ LiteLLM 导入成功，回调已设置")
        print(f"   Success callbacks: {litellm.success_callback}")
        print(f"   Failure callbacks: {litellm.failure_callback}")

        # 测试一个简单的 LLM 调用
        print("\n🔄 发送测试请求...")

        response = litellm.completion(
            model="gpt-3.5-turbo",
            messages=[
                {"role": "system", "content": "You are a helpful assistant."},
                {
                    "role": "user",
                    "content": "Say 'Hello from LiteLLM + Langfuse test!'",
                },
            ],
            max_tokens=50,
            stream=False,  # 使用非流式调用以便正确访问响应
        )

        print("✅ LiteLLM 调用成功")

        # 简化响应处理，避免类型错误
        try:
            message_content = str(response.choices[0].message.content)
            print(f"   响应: {message_content}")
        except (AttributeError, IndexError) as e:
            print(f"   响应格式异常: {e}")
            print(f"   原始响应类型: {type(response)}")
            return False

        # 等待一下让 Langfuse 处理数据
        print("\n⏳ 等待 Langfuse 处理数据...")
        import time

        time.sleep(2)

        # 尝试手动刷新 Langfuse
        try:
            from langfuse import Langfuse

            langfuse = Langfuse()
            langfuse.flush()
            print("✅ Langfuse 数据已刷新")
        except Exception as e:
            print(f"⚠️  Langfuse 刷新时出错: {e}")

        print("\n🎉 测试完成！")
        print("💡 请检查你的 Langfuse 仪表板: https://us.cloud.langfuse.com")
        print("   你应该能看到一个新的 trace 记录。")

        return True

    except Exception as e:
        print(f"❌ 测试失败: {e}")
        import traceback

        traceback.print_exc()
        return False


@pytest.mark.filterwarnings("ignore:Pydantic serializer warnings:UserWarning")
def test_litellm_langfuse_integration():
    """测试 LiteLLM 与 Langfuse 的集成"""
    required_vars = [
        "OPENAI_API_KEY",
        "LANGFUSE_PUBLIC_KEY",
        "LANGFUSE_SECRET_KEY",
        "LANGFUSE_HOST",
    ]
    missing_vars = _missing_env_vars(required_vars)
    if missing_vars:
        pytest.skip(f"缺少环境变量: {missing_vars}")

    print("✅ 环境变量检查通过")
    assert _run_litellm_langfuse_integration()


async def _runshell_integration() -> str:
    """Run Shell LLM integration test and return response."""
    print("\n🧪 测试 Shell LLM 集成")
    print("=" * 50)

    try:
        from aish.config import ConfigModel
        from aish.llm import LLMSession
        from aish.skills import SkillManager

        config = ConfigModel(
            model="gpt-3.5-turbo", api_key=os.getenv("OPENAI_API_KEY"), api_base=None
        )

        skill_manager = SkillManager()
        skill_manager.load_all_skills()
        llm_session = LLMSession(config=config, skill_manager=skill_manager)

        print("✅ Shell LLMSession 创建成功")

        # 测试一个简单的调用
        print("🔄 测试 LLM 调用...")
        response = await llm_session.completion(
            prompt="说 'Hello from Shell test!'",
            system_message="You are a helpful assistant.",
        )

        print("✅ Shell LLM 调用成功")
        print(f"   响应: {response[:100]}...")
        return response

    except Exception as e:
        print(f"❌ Shell 测试失败: {e}")
        import traceback

        traceback.print_exc()
        return ""


@pytest.mark.asyncio
async def testshell_integration():
    """测试 Shell 的 LLM 集成"""
    required_vars = ["OPENAI_API_KEY"]
    missing_vars = _missing_env_vars(required_vars)
    if missing_vars:
        pytest.skip(f"缺少环境变量: {missing_vars}")

    response = await _runshell_integration()
    assert response


def main():
    print("🚀 LiteLLM + Langfuse 集成测试")

    # 检查 API 密钥
    if not os.getenv("OPENAI_API_KEY"):
        print("⚠️  警告: 未设置 OPENAI_API_KEY，某些测试可能失败")
        print("   请设置: export OPENAI_API_KEY='your-api-key'")

    # 运行测试
    required_langfuse_vars = [
        "OPENAI_API_KEY",
        "LANGFUSE_PUBLIC_KEY",
        "LANGFUSE_SECRET_KEY",
        "LANGFUSE_HOST",
    ]
    missing_langfuse = _missing_env_vars(required_langfuse_vars)
    if missing_langfuse:
        print(f"❌ 缺少环境变量: {missing_langfuse}")
        test1 = False
    else:
        test1 = _run_litellm_langfuse_integration()

    if not os.getenv("OPENAI_API_KEY"):
        test2 = False
    else:
        test2 = asyncio.run(_runshell_integration())

    print("\n" + "=" * 50)
    print("📊 测试总结:")
    print(f"   LiteLLM + Langfuse: {'✅' if test1 else '❌'}")
    print(f"   Shell 集成: {'✅' if test2 else '❌'}")

    if test1 and test2:
        print("\n🎉 所有测试通过！")
        print("💡 如果你在 Langfuse 仪表板中看到了 traces，说明集成工作正常。")
    else:
        print("\n❌ 部分测试失败，请检查配置和网络连接。")


if __name__ == "__main__":
    main()
