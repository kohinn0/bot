"""
Non-blocking logger for async bot.
A RotatingFileHandler SZINKRON – minden write() blokkolja az asyncio loopot.
Megoldás: QueueHandler + QueueListener (Python 3.12+ kompatibilis).
A QueueListener külön szálban írja a fájlt, a fő loop nem blokkolódik.
"""
import os
import logging
import queue
from logging.handlers import RotatingFileHandler, QueueHandler, QueueListener


def get_logger(name: str = "CryptoBot") -> logging.Logger:
    log_dir = "logs"
    if not os.path.exists(log_dir):
        os.makedirs(log_dir)

    logger = logging.getLogger(name)
    logger.setLevel(logging.INFO)

    if logger.handlers:
        return logger  # Már inicializálva

    formatter = logging.Formatter(
        "%(asctime)s | %(levelname)-7s | %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )

    # ── Tényleges handlerek (a háttérszálban futnak) ──────────────────
    file_handler = RotatingFileHandler(
        os.path.join(log_dir, "bot.log"),
        maxBytes=10 * 1024 * 1024,
        backupCount=5,
        encoding="utf-8",
    )
    file_handler.setFormatter(formatter)

    console_handler = logging.StreamHandler()
    console_handler.setFormatter(formatter)

    # ── Aszinkron queue: a logger NEM blokkolja az event loopot ───────
    log_queue: queue.Queue = queue.Queue(maxsize=10_000)

    # QueueListener: háttérszálban írja a handlerekbe a bejövő bejegyzéseket
    listener = QueueListener(
        log_queue,
        file_handler,
        console_handler,
        respect_handler_level=True,
    )
    listener.start()

    # A logger csak annyit csinál, hogy beteszi az üzenetet a sorba (O(1), nem blokkol)
    queue_handler = QueueHandler(log_queue)
    logger.addHandler(queue_handler)

    # A listener leállítása a folyamat végén
    import atexit
    atexit.register(listener.stop)

    return logger


logger = get_logger()
