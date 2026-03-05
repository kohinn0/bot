import os
import glob
import pandas as pd
import numpy as np

def calculate_toxicity_score(markouts_1s, window=50):
    scores = []
    # Using a rolling window
    for i in range(len(markouts_1s)):
        # Calculate for window up to current (exclusive or inclusive? the engine adds to deque then calculates, so inclusive)
        start_idx = max(0, i - window + 1)
        window_slice = markouts_1s[start_idx:i+1]
        
        # Valid numerical markouts
        valid = [m for m in window_slice if not np.isnan(m)]
        if not valid:
            scores.append(0.0)
            continue
            
        negative_count = sum(1 for m in valid if m < 0)
        ratio = negative_count / len(valid)
        score = min(1.0, ratio * 1.5)
        scores.append(score)
        
    return scores

def main():
    log_dir = "logs"
    csv_files = glob.glob(os.path.join(log_dir, "toxicity_log_*.csv"))
    
    if not csv_files:
        print("Még nincs CSV log a 'logs' könyvtárban.")
        return
        
    latest_csv = max(csv_files, key=os.path.getmtime)
    print(f"Elemzés: {latest_csv}\n")
    
    try:
        df = pd.read_csv(latest_csv)
    except pd.errors.EmptyDataError:
        print("A CSV fájl üres (csak fejléc van). Még nem regisztráltunk fillt.")
        return
        
    if len(df) == 0:
        print("A CSV fájlban nincsenek adatsorok (0 fill).")
        return
        
    # N_fills
    n_fills = len(df)
    
    print("="*50)
    print(" 🎯 A) CORE METRIKÁK")
    print("="*50)
    print(f"N_fills: {n_fills}")
    
    # 1s markout
    m1s = df['markout_1s_micro'].dropna()
    if len(m1s) > 0:
        p_neg_1s = (m1s < 0).mean() * 100
        p10_1s, p50_1s, p90_1s = np.percentile(m1s, [10, 50, 90])
        print(f"\n--- 1 Másodperces Markout ---")
        print(f"p_neg_1s: {p_neg_1s:.1f}% toxikus")
        print(f"median(markout_1s_micro): {p50_1s:.5f}")
        print(f"p10: {p10_1s:.5f} | p50: {p50_1s:.5f} | p90: {p90_1s:.5f}")
    else:
        print("\nNincs elég 1s markout adat.")
        
    # 250ms markout
    m250 = df['markout_250ms_micro'].dropna()
    if len(m250) > 0:
        p_neg_250 = (m250 < 0).mean() * 100
        p10_250, p50_250, p90_250 = np.percentile(m250, [10, 50, 90])
        print(f"\n--- 250ms Markout ---")
        print(f"p_neg_250ms: {p_neg_250:.1f}% toxikus")
        print(f"median(markout_250ms_micro): {p50_250:.5f}")
        print(f"p10: {p10_250:.5f} | p50: {p50_250:.5f} | p90: {p90_250:.5f}")
        
    # 5s markout
    m5s = df['markout_5s_micro'].dropna()
    if len(m5s) > 0:
        p_neg_5s = (m5s < 0).mean() * 100
        p10_5s, p50_5s, p90_5s = np.percentile(m5s, [10, 50, 90])
        print(f"\n--- 5 Másodperces Markout ---")
        print(f"p_neg_5s: {p_neg_5s:.1f}% toxikus")
        print(f"median(markout_5s_micro): {p50_5s:.5f}")
        print(f"p10: {p10_5s:.5f} | p50: {p50_5s:.5f} | p90: {p90_5s:.5f}")

    print("\n" + "="*50)
    print(" ⏱️  B) LATENCY SANITY")
    print("="*50)
    
    lat_sig_quote = df['latency_signal_to_quote_ms'].dropna()
    lat_quote_fill = df['latency_quote_to_fill_ms'].dropna()
    
    if len(lat_sig_quote) > 0:
        med_sig = np.median(lat_sig_quote)
        p95_sig = np.percentile(lat_sig_quote, 95)
        print(f"Signal -> Quote Latency:")
        print(f"  Median: {med_sig:.0f} ms")
        print(f"  p95:    {p95_sig:.0f} ms")
        
    if len(lat_quote_fill) > 0:
        med_fill = np.median(lat_quote_fill)
        p95_fill = np.percentile(lat_quote_fill, 95)
        print(f"\nQuote -> Fill Latency:")
        print(f"  Median: {med_fill:.0f} ms")
        print(f"  p95:    {p95_fill:.0f} ms")

    print("\n" + "="*50)
    print(" ☢️  C) TOXICITY SCORE VALIDÁCIÓ")
    print("="*50)
    
    if len(m1s) > 0:
        df['implied_toxicity_score'] = calculate_toxicity_score(df['markout_1s_micro'].values)
        
        bins = [0, 0.3, 0.6, 1.0]
        labels = ['[0.0 - 0.3]', '(0.3 - 0.6]', '(0.6 - 1.0]']
        
        df['tox_bin'] = pd.cut(df['implied_toxicity_score'], bins=bins, labels=labels, include_lowest=True)
        
        for bin_label in labels:
            bin_data = df[df['tox_bin'] == bin_label]['markout_1s_micro'].dropna()
            count = len(bin_data)
            if count == 0:
                print(f"Bin {bin_label}: Nincs adat")
                continue
                
            p_neg = (bin_data < 0).mean() * 100
            median_m1s = np.median(bin_data)
            
            print(f"Bin {bin_label} (N={count}):")
            print(f"  -> p_neg_1s: {p_neg:.1f}%")
            print(f"  -> median_markout_1s: {median_m1s:.5f}\n")
            
    print("====================================================\n")

if __name__ == "__main__":
    main()
