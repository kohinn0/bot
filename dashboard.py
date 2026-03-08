import time
import json
import os
import plotext as plt
from datetime import datetime

STATE_FILE = "logs/pnl_state.json"

def clear_screen():
    os.system('cls' if os.name == 'nt' else 'clear')

def load_state():
    if not os.path.exists(STATE_FILE):
        return None
    try:
        with open(STATE_FILE, 'r') as f:
            return json.load(f)
    except Exception:
        return None

def draw_dashboard():
    state = load_state()
    clear_screen()
    
    print("="*60)
    print(f"🚀 SEBESSEG BOT - ÉLŐ PnL DASHBOARD 🚀")
    print(f"🕒 Utolsó frissítés: {datetime.now().strftime('%H:%M:%S')}")
    print("="*60)
    
    if not state:
        print("\n⏳ Várakozás az első Trade adatokra a bot-tól...")
        print("💡 Tipp: Ellenőrizd, hogy fut-e a bot.py!")
        return

    # Adatok kinyerése
    cum_pnl = state.get("cumulative_pnl", 0.0)
    wins = state.get("win_count", 0)
    losses = state.get("loss_count", 0)
    fees = state.get("total_fees", 0.0)
    account_value = state.get("account_value", 0.0)
    trades_history = state.get("trades", [0.0])
    
    total_trades = wins + losses
    win_rate = (wins / total_trades * 100) if total_trades > 0 else 0.0
    
    # 🌟 Napi Statisztikák 
    print(f"💸 Jelenlegi Egyenleg: ${account_value + cum_pnl:.2f} USD")
    print(f"💎 Napi Tiszta Profit: ${cum_pnl:.2f} USD")
    print("-" * 60)
    print(f"📈 Kötések száma:   {total_trades}")
    print(f"🏆 Win Rate:        {win_rate:.1f}% ({wins} Win / {losses} Loss)")
    print(f"📉 Taker Fee -k:     ${fees:.2f}")
    print("="*60)
    
    # 📊 Grafikon (ha van már legalább 1 lezárt kötés)
    if len(trades_history) > 1:
        try:
            plt.clear_figure()
            
            # Formázás beállítása
            plt.theme("dark")
            color = "green" if cum_pnl >= 0 else "red"
            
            plt.plot(trades_history, marker="dot", color=color)
            plt.title("📈 Kummulált Net PnL (USD)")
            plt.xlabel("Trade Sorszám")
            plt.ylabel("USD ($)")
            
            # X tengely igazítása a valós ticks-hez (hogy ne legyenek tizedes kötések)
            plt.xticks(list(range(len(trades_history))))
            
            # Grafikon kiírása
            print("\n")
            plt.show()
        except Exception as e:
            print(f"\n⚠️ Grafikon rajzolási hiba: {e}")
    else:
        print("\n📊 Grafikon: Még nincs elég adat a kirajzoláshoz.")
        
    print("\n" + "="*60)
    print("🔄 Frissítés 2 másodpercenként... (Kilépés: CTRL+C)")


if __name__ == "__main__":
    try:
        while True:
            draw_dashboard()
            time.sleep(2)
    except KeyboardInterrupt:
        print("\n👋 Dashboard leállítva.")
