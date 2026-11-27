import os
from pathlib import Path


class Server:
    def serve(self, port: int) -> None:
        bind(port)


def bind(port):
    return os.getpid() + port


PORT = 8080
