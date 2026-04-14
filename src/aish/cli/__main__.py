"""Main entry point for python -m aish.cli"""

from aish.cli import main

if __name__ == "__main__":
    from dotenv import load_dotenv

    load_dotenv()
    main()
