#!/usr/bin/env python3
"""Reconcile recent on-chain PumpSwap trades and fields with amm_trades rows.

Uses only the Python standard library and `psql`. The audit intentionally
recognizes every current price-changing PumpSwap instruction, independent of
the collector's whitelist, pool, quote-mint, or CPI-depth filters.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import time
import urllib.request
from collections import Counter, defaultdict


PUMP_AMM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
PUMP_PROGRAM = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
WSOL = "So11111111111111111111111111111111111111112"
USDC = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
DISCRIMINATORS = {
    bytes([102, 6, 61, 18, 1, 218, 235, 234]): ("buy", True),
    bytes([198, 46, 21, 82, 180, 217, 232, 112]): ("buy_exact_quote_in", True),
    bytes([51, 230, 133, 164, 1, 127, 131, 173]): ("sell", False),
    bytes([105, 68, 6, 175, 0, 7, 35, 162]): ("boost_buy_and_burn", True),
}
EVENT_DISCRIMINATORS = {
    "buy": bytes([103, 244, 82, 31, 44, 245, 119, 119]),
    "buy_exact_quote_in": bytes([103, 244, 82, 31, 44, 245, 119, 119]),
    "sell": bytes([62, 47, 55, 10, 165, 3, 220, 42]),
    "boost_buy_and_burn": bytes([63, 69, 28, 22, 48, 92, 194, 185]),
}


def u64(raw: bytes, offset: int) -> int | None:
    end = offset + 8
    return int.from_bytes(raw[offset:end], "little") if len(raw) >= end else None


def b58decode(value: str) -> bytes:
    number = 0
    for char in value:
        number = number * 58 + ALPHABET.index(char)
    raw = number.to_bytes((number.bit_length() + 7) // 8, "big") if number else b""
    return b"\0" * (len(value) - len(value.lstrip("1"))) + raw


def b58encode(raw: bytes) -> str:
    number = int.from_bytes(raw, "big")
    encoded = ""
    while number:
        number, remainder = divmod(number, 58)
        encoded = ALPHABET[remainder] + encoded
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + encoded


def is_ed25519_point(encoded: bytes) -> bool:
    """Equivalent to Solana's compressed-Edwards on-curve PDA rejection."""
    if len(encoded) != 32:
        return False
    prime = 2**255 - 19
    sign = encoded[31] >> 7
    y = int.from_bytes(encoded, "little") & ((1 << 255) - 1)
    if y >= prime:
        return False
    d = (-121665 * pow(121666, prime - 2, prime)) % prime
    y_squared = y * y % prime
    denominator = (d * y_squared + 1) % prime
    if denominator == 0:
        return False
    x_squared = (y_squared - 1) * pow(denominator, prime - 2, prime) % prime
    x = pow(x_squared, (prime + 3) // 8, prime)
    if x * x % prime != x_squared:
        x = x * pow(2, (prime - 1) // 4, prime) % prime
    if x * x % prime != x_squared or (x == 0 and sign == 1):
        return False
    return True


def find_program_address(seeds: list[bytes], program: str) -> bytes:
    program_bytes = b58decode(program)
    for bump in range(255, -1, -1):
        candidate = hashlib.sha256(
            b"".join(seeds + [bytes([bump]), program_bytes, b"ProgramDerivedAddress"])
        ).digest()
        if not is_ed25519_point(candidate):
            return candidate
    raise RuntimeError("unable to derive program address")


def canonical_pool(base_mint: str, quote_mint: str) -> str:
    base = b58decode(base_mint)
    quote = b58decode(quote_mint)
    authority = find_program_address([b"pool-authority", base], PUMP_PROGRAM)
    pool = find_program_address(
        [b"pool", (0).to_bytes(2, "little"), authority, base, quote], PUMP_AMM
    )
    return b58encode(pool)


class Rpc:
    def __init__(self, endpoint: str) -> None:
        self.endpoint = endpoint

    def call(self, method: str, params: list, attempts: int = 8):
        payload = json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).encode()
        for attempt in range(attempts):
            try:
                request = urllib.request.Request(
                    self.endpoint,
                    data=payload,
                    headers={"content-type": "application/json", "user-agent": "curl/8.5.0"},
                )
                with urllib.request.urlopen(request, timeout=30) as response:
                    result = json.load(response)
                if result.get("error"):
                    raise RuntimeError(result["error"])
                return result.get("result")
            except Exception:
                if attempt + 1 == attempts:
                    raise
                time.sleep(min(0.5 * (2**attempt), 8))


def load_database_url(env_file: str) -> str:
    with open(env_file, encoding="utf-8") as handle:
        for raw_line in handle:
            key, separator, value = raw_line.strip().partition("=")
            if separator and key == "PG_DATABASE_URL":
                return value.strip().strip('"').strip("'")
    raise RuntimeError(f"PG_DATABASE_URL not found in {env_file}")


def database_whitelist(database_url: str) -> set[str]:
    result = subprocess.run(
        [
            "psql",
            "-d",
            database_url,
            "-v",
            "ON_ERROR_STOP=1",
            "-At",
            "-c",
            "SELECT token_address FROM all_mints WHERE whitelisted = true",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return set(result.stdout.splitlines())


def database_rows(database_url: str, signatures: list[str]):
    if not signatures:
        return []
    quoted = ",".join("'" + signature + "'" for signature in signatures)
    sql = (
        "SELECT tx_signature,ix_index,mint_address,is_buy,success,"
        "to_jsonb(amm_trades)->>'pool_address',"
        "to_jsonb(amm_trades)->>'quote_mint_address',"
        "to_jsonb(amm_trades)->>'instruction_type',"
        "to_jsonb(amm_trades)->>'amount_source',"
        "to_jsonb(amm_trades)->>'is_cpi',"
        "to_jsonb(amm_trades)->>'user_pubkey',"
        "to_jsonb(amm_trades)->>'token_amount',"
        "to_jsonb(amm_trades)->>'sol_amount' "
        f"FROM amm_trades WHERE tx_signature IN ({quoted}) "
        "ORDER BY tx_signature,ix_index"
    )
    result = subprocess.run(
        ["psql", "-d", database_url, "-v", "ON_ERROR_STOP=1", "-At", "-F", "|", "-c", sql],
        env=os.environ.copy(),
        check=True,
        capture_output=True,
        text=True,
    )
    rows = []
    for line in result.stdout.splitlines():
        (
            signature,
            ix_index,
            mint,
            is_buy,
            success,
            pool,
            quote_mint,
            kind,
            amount_source,
            is_cpi,
            user,
            token_amount,
            quote_amount,
        ) = line.split("|")
        rows.append(
            {
                "signature": signature,
                "ix_index": int(ix_index),
                "mint": mint,
                "is_buy": is_buy == "t",
                "success": success == "t",
                "pool": pool or None,
                "quote_mint": quote_mint or None,
                "kind": kind or None,
                "amount_source": amount_source or None,
                "is_cpi": is_cpi == "true" if is_cpi else None,
                "user": user or None,
                "token_amount": int(token_amount) if token_amount else None,
                "quote_amount": int(quote_amount) if quote_amount else None,
            }
        )
    return rows


def transaction_trades(signature: str, transaction: dict) -> list[dict]:
    message = transaction["transaction"]["message"]
    meta = transaction["meta"]
    loaded = meta.get("loadedAddresses") or {}
    keys = (
        message["accountKeys"]
        + loaded.get("writable", [])
        + loaded.get("readonly", [])
    )
    program_index = keys.index(PUMP_AMM)
    success = meta["err"] is None
    trades = []
    inner_blocks = {
        block["index"]: block["instructions"]
        for block in (meta.get("innerInstructions") or [])
    }

    def inspect(
        instruction: dict,
        path: str,
        depth: str,
        parent_outer_index: int,
        start_inner_position: int,
        trade_stack: int,
    ) -> None:
        if instruction["programIdIndex"] != program_index:
            return
        decoded = b58decode(instruction["data"])
        kind = DISCRIMINATORS.get(decoded[:8])
        if kind is None:
            return
        name, is_buy = kind
        accounts = instruction["accounts"]
        mint = keys[accounts[3]] if len(accounts) > 3 else None
        quote_mint = keys[accounts[4]] if len(accounts) > 4 else None
        pool = keys[accounts[0]] if accounts else None
        user = keys[accounts[1]] if len(accounts) > 1 else None

        # Failed trades and the rare successful call without a decodable event
        # retain submitted bounds, explicitly labeled as non-executed amounts.
        arg0 = u64(decoded, 8) or 0
        arg1 = u64(decoded, 16) or 0
        exact_in = name in {"buy_exact_quote_in", "boost_buy_and_burn"}
        token_amount, quote_amount = (arg1, arg0) if exact_in else (arg0, arg1)
        amount_source = "instruction_request"

        if success:
            expected_event_disc = EVENT_DISCRIMINATORS[name]
            for candidate in inner_blocks.get(parent_outer_index, [])[start_inner_position:]:
                height = candidate.get("stackHeight")
                if height is not None and height <= trade_stack:
                    break
                if height is not None and height != trade_stack + 1:
                    continue
                if candidate["programIdIndex"] != program_index:
                    continue
                event_instruction = b58decode(candidate["data"])
                if event_instruction[8:16] != expected_event_disc:
                    continue
                event = event_instruction[8:]
                if name == "boost_buy_and_burn":
                    parsed_token = u64(event, 160)
                    parsed_quote = u64(event, 152)
                else:
                    parsed_token = u64(event, 16)
                    parsed_quote = u64(event, 112 if is_buy else 64)
                if parsed_token is not None and parsed_quote is not None:
                    token_amount = parsed_token
                    quote_amount = parsed_quote
                    amount_source = "event"
                break

        trades.append(
            {
                "signature": signature,
                "path": path,
                "depth": depth,
                "kind": name,
                "is_buy": is_buy,
                "success": success,
                "mint": mint,
                "quote_mint": quote_mint,
                "pool": pool,
                "user": user,
                "token_amount": token_amount,
                "quote_amount": quote_amount,
                "amount_source": amount_source,
                "is_cpi": depth != "outer",
            }
        )

    for outer_index, instruction in enumerate(message["instructions"]):
        inspect(instruction, f"o:{outer_index}", "outer", outer_index, 0, 1)
    for block in meta.get("innerInstructions") or []:
        for inner_index, instruction in enumerate(block["instructions"]):
            inspect(
                instruction,
                f"i:{block['index']}:{inner_index}",
                f"cpi_stack_{instruction.get('stackHeight', 'unknown')}",
                block["index"],
                inner_index + 1,
                instruction.get("stackHeight") or 1,
            )
    return trades


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rpc", default="https://solana-rpc.publicnode.com")
    parser.add_argument("--limit", type=int, default=100)
    parser.add_argument(
        "--skip-newest",
        type=int,
        default=100,
        help="Skip newest signatures so the collector's batch flush can settle",
    )
    parser.add_argument("--delay", type=float, default=0.08)
    parser.add_argument("--env-file", default=".env")
    parser.add_argument(
        "--all-pools",
        action="store_true",
        help="Diagnostic mode: ignore production whitelist/canonical/quote/base filters",
    )
    args = parser.parse_args()

    rpc = Rpc(args.rpc)
    database_url = load_database_url(args.env_file)
    whitelist = database_whitelist(database_url)
    signature_rows = rpc.call(
        "getSignaturesForAddress",
        [
            PUMP_AMM,
            {"limit": args.limit + args.skip_newest, "commitment": "finalized"},
        ],
    )
    signature_rows = signature_rows[args.skip_newest :]
    signatures = [row["signature"] for row in signature_rows]
    expected = []
    for index, signature in enumerate(signatures, 1):
        transaction = rpc.call(
            "getTransaction",
            [
                signature,
                {
                    "encoding": "json",
                    "maxSupportedTransactionVersion": 0,
                    "commitment": "finalized",
                },
            ],
        )
        if transaction is not None:
            trades = transaction_trades(signature, transaction)
            if not args.all_pools:
                trades = [
                    trade
                    for trade in trades
                    if trade["mint"] != WSOL
                    and trade["quote_mint"] in {WSOL, USDC}
                    and trade["mint"] in whitelist
                    and trade["pool"]
                    == canonical_pool(trade["mint"], trade["quote_mint"])
                ]
            expected.extend(trades)
        if index % 20 == 0:
            print(f"fetched {index}/{len(signatures)} transactions", flush=True)
        time.sleep(args.delay)

    rows = database_rows(database_url, signatures)
    actual_by_signature = defaultdict(list)
    for row in rows:
        actual_by_signature[row["signature"]].append(row)
    expected_by_signature = defaultdict(list)
    for trade in expected:
        expected_by_signature[trade["signature"]].append(trade)

    missing = []
    excess = []
    mismatched = []
    for signature in signatures:
        chain = expected_by_signature[signature]
        database = actual_by_signature[signature]
        if len(database) < len(chain):
            missing.extend(chain[len(database) :])
        elif len(database) > len(chain):
            excess.extend(database[len(chain) :])
        for index, (chain_trade, database_trade) in enumerate(zip(chain, database)):
            expected_fields = {
                "mint": chain_trade["mint"],
                "is_buy": chain_trade["is_buy"],
                "success": chain_trade["success"],
                "pool": chain_trade["pool"],
                "quote_mint": chain_trade["quote_mint"],
                "kind": chain_trade["kind"],
                "amount_source": chain_trade["amount_source"],
                "is_cpi": chain_trade["is_cpi"],
                "user": chain_trade["user"],
                "token_amount": chain_trade["token_amount"],
                "quote_amount": chain_trade["quote_amount"],
            }
            actual_fields = {key: database_trade[key] for key in expected_fields}
            if database_trade["ix_index"] != index or actual_fields != expected_fields:
                mismatched.append(
                    {
                        "signature": signature,
                        "ix_index": index,
                        "chain": expected_fields,
                        "database": actual_fields,
                    }
                )

    print("\ncoverage summary")
    print("transactions sampled", len(signatures))
    print("on-chain trade instructions", len(expected))
    print("database rows", len(rows))
    print("missing rows", len(missing))
    print("excess rows", len(excess))
    print("field-mismatched rows", len(mismatched))
    print("on-chain by kind", dict(Counter(x["kind"] for x in expected)))
    print("on-chain by depth", dict(Counter(x["depth"] for x in expected)))
    print("missing by kind", dict(Counter(x["kind"] for x in missing)))
    print("missing by depth", dict(Counter(x["depth"] for x in missing)))
    print("missing by success", dict(Counter(x["success"] for x in missing)))
    print("missing examples")
    for trade in missing[:20]:
        print(json.dumps(trade, sort_keys=True))
    print("field mismatch examples")
    for mismatch in mismatched[:20]:
        print(json.dumps(mismatch, sort_keys=True))

    if missing or excess or mismatched:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
