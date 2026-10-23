"""Refresh the checked-in EC2 on-demand price table the cost estimator uses.

Fetches us-east-1 Linux on-demand hourly prices (shared tenancy, no pre-installed software) for
every instance type from the AWS Pricing API (public price data; any AWS credentials work) and
writes crates/biffix-ingest/data/ec2_ondemand_us-east-1.json as {instance_type: usd_per_hour}.
Other regions' prices are not checked in: the reconcile Lambda reads them from the Pricing API, with
the same filters, as its jobs need them.

Requires the aws CLI and AWS credentials.

Usage: python3 crates/biffix-ingest/scripts/refresh_ec2_ondemand_prices.py
"""

import json
import subprocess
import sys
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "data/ec2_ondemand_us-east-1.json"

filters = [
    ("TERM_MATCH", "location", "US East (N. Virginia)"),
    ("TERM_MATCH", "operatingSystem", "Linux"),
    ("TERM_MATCH", "tenancy", "Shared"),
    ("TERM_MATCH", "preInstalledSw", "NA"),
    ("TERM_MATCH", "capacitystatus", "Used"),
    ("TERM_MATCH", "licenseModel", "No License required"),
]
f_args = [f"Type={t},Field={k},Value={v}" for t, k, v in filters]
prices = {}
token = None
while True:
    cmd = ["aws", "pricing", "get-products", "--region", "us-east-1", "--service-code", "AmazonEC2",
           "--max-results", "100", "--output", "json", "--filters", *f_args]
    if token:
        cmd += ["--next-token", token]
    try:
        result = subprocess.run(cmd, check=True, capture_output=True, text=True)
    except subprocess.CalledProcessError as e:
        sys.exit(f"aws pricing get-products failed ({e.returncode}): {e.stderr.strip()}")
    out = json.loads(result.stdout)
    for item in out["PriceList"]:
        p = json.loads(item)
        itype = p["product"]["attributes"].get("instanceType")
        for term in p.get("terms", {}).get("OnDemand", {}).values():
            for dim in term["priceDimensions"].values():
                if dim.get("unit") == "Hrs":
                    usd = float(dim["pricePerUnit"]["USD"])
                    if usd > 0 and itype:
                        prices[itype] = usd
    token = out.get("NextToken")
    if not token:
        break
OUT.write_text(json.dumps(dict(sorted(prices.items())), indent=1) + "\n")
print(f"wrote {len(prices)} instance types to {OUT}")
