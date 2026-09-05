#!/usr/bin/env python3
import sys
import re

def parse_frconn(path):
    total_expansions = 0
    total_search_us = 0
    total_init_us = 0
    attempts = 0
    with open(path) as f:
        for line in f:
            if not line.startswith("FRCONN net="):
                continue
            m = re.search(r"expansions=(\d+), search_rooms=\d+, search_us=(\d+)", line)
            m_init = re.search(r"init_us=(\d+)", line)
            if m:
                exp = int(m.group(1))
                search_us = int(m.group(2))
                init_us = int(m_init.group(1)) if m_init else 0
                total_expansions += exp
                total_search_us += search_us
                total_init_us += init_us
                attempts += 1
    weighted_us_per_exp = (total_search_us / total_expansions) if total_expansions > 0 else 0
    print(f"File: {path}")
    print(f"Attempts: {attempts}")
    print(f"Expansions: {total_expansions}")
    print(f"Search us: {total_search_us} ({total_search_us / 1e6:.3f} s)")
    print(f"Init us: {total_init_us} ({total_init_us / 1e6:.3f} s)")
    print(f"Weighted µs/exp: {weighted_us_per_exp:.2f}")
    return weighted_us_per_exp, total_expansions, total_search_us

if __name__ == "__main__":
    if len(sys.argv) > 1:
        parse_frconn(sys.argv[1])
