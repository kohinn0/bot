import time, requests, os
from dotenv import load_dotenv
load_dotenv()

session = requests.Session()
session.headers.update({'User-Agent': 'Mozilla/5.0', 'Accept': 'application/json'})

print('=' * 55)
print('  POLYMARKET KAPCSOLAT TESZT')
print('=' * 55)

# 1. CLOB API - piacok listázása
print('\n[1] CLOB API - Markets endpoint')
try:
    start = time.perf_counter()
    r = session.get('https://clob.polymarket.com/markets?limit=1', timeout=10)
    ms = (time.perf_counter() - start) * 1000
    print(f'    HTTP {r.status_code} | {ms:.0f}ms')
    if r.status_code == 200:
        data = r.json()
        print(f'    ✅ Működik! {len(data["data"])} piac')
except Exception as e:
    print(f'    ❌ Hiba: {e}')

# 2. Gamma API
print('\n[2] GAMMA API - BTC 15-min piacok')
try:
    start = time.perf_counter()
    r2 = session.get('https://gamma-api.polymarket.com/events?closed=false&limit=5&tag=crypto', timeout=10)
    ms2 = (time.perf_counter() - start) * 1000
    print(f'    HTTP {r2.status_code} | {ms2:.0f}ms')
except Exception as e:
    print(f'    ❌ Hiba: {e}')

# 3. API kulcsok teszt
print('\n[3] POLYMARKET API KULCSOK')
api_key = os.getenv('POLYMARKET_API_KEY', '')
pk = os.getenv('PRIVATE_KEY', '')
funder = os.getenv('FUNDER_ADDRESS', '')
print(f'    ✅ Kulcsok OK: API={bool(api_key)}, PK={bool(pk)}, Funder={bool(funder)}')

# 4. Trading API auth teszt
print('\n[4] TRADING API - Auth teszt')
try:
    from py_clob_client.client import ClobClient
    print('    -> ClobClient letrehozasa...')
    client = ClobClient('https://clob.polymarket.com', key=pk, chain_id=137, funder=funder, signature_type=1)
    print('    -> create_or_derive_api_creds() hívása... VÁRJ (ez eltarthat egy ideig!)...')
    creds = client.create_or_derive_api_creds()
    print(f'    ✅ API Creds generálva!')
    client.creds = creds
    print('    -> get_orders() hívása...')
    orders = client.get_orders()
    print(f'    ✅ Open orders: {len(orders) if orders else 0} db')
except Exception as e:
    print(f'    ❌ Hiba: {e}')

print('\n' + '=' * 55)
print('POLYMARKET_CONNECTION_TEST_DONE')
