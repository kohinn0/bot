import json
import os
from bot_logger import logger

STATE_FILE = "inventory_state.json"

def read_inventory():
    """Returns (inventory_yes, inventory_no) from state file."""
    if not os.path.exists(STATE_FILE):
        return 0, 0
    try:
        with open(STATE_FILE, "r") as f:
            data = json.load(f)
            return int(data.get("yes", 0)), int(data.get("no", 0))
    except Exception as e:
        logger.error(f"Failed to read inventory state: {e}")
        return 0, 0

def save_inventory(yes, no):
    """Saves (yes, no) to state file."""
    temp_file = STATE_FILE + ".tmp"
    try:
        with open(temp_file, "w") as f:
            json.dump({"yes": yes, "no": no}, f)
        os.replace(temp_file, STATE_FILE)
    except Exception as e:
        logger.error(f"Failed to save inventory state: {e}")
