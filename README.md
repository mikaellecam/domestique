# whoosh2garmin

Syncs your [MyWhoosh](https://mywhoosh.com) rides to Garmin Connect, rewritten as if a Garmin
device had recorded them, so Garmin computes Training Effect and Training Load for them.

For each ride Garmin doesn't have yet, it:

1. downloads the original FIT file from your MyWhoosh account,
2. sets the device to the Garmin model you choose,
3. adds the lap MyWhoosh leaves out after the last workout step (the cool-down),
4. uploads it to Garmin Connect.

A ride counts as already on Garmin when a Garmin activity covers its start time, including the
same ride recorded on a watch, so nothing is counted twice. There is no local state: Garmin's
activity list is the record, and a failed run is retried by the next one.

> **Unofficial.** Neither MyWhoosh nor Garmin offers an API for this. The tool uses the private
> APIs their own apps use, so it can break whenever either changes them, and using it may go
> against their terms. Use it with your own accounts only.

## Install

```sh
cargo install --path .
```

## Use

```sh
whoosh2garmin login                                  # once
whoosh2garmin sync --device edge_explore2 --dry-run  # see what would be uploaded
whoosh2garmin sync --device edge_explore2            # last 7 days; --days 90 to backfill
```

`login` asks for your MyWhoosh email and password, then opens Garmin's sign-in page in your
browser. Your Garmin password and MFA code are typed on Garmin's page only; the tool receives
tokens, which it renews by itself.

`whoosh2garmin devices` lists the device names. Pick a model that has Garmin's training
metrics (most Edge, Forerunner and Fenix models do).

`whoosh2garmin rewrite in.fit out.fit --device edge_explore2` rewrites one file without
uploading it.

Credentials are stored in plain text, readable only by you, in:

- Linux: `~/.config/whoosh2garmin/credentials.json`
- macOS: `~/Library/Application Support/whoosh2garmin/credentials.json`
- Windows: `%APPDATA%\whoosh2garmin\credentials.json`

## Run it on a schedule

- Linux and macOS (`crontab -e`), every hour:
  `0 * * * * $HOME/.cargo/bin/whoosh2garmin sync --device edge_explore2`
- Windows, every hour:
  `schtasks /create /sc hourly /tn whoosh2garmin /tr "%USERPROFILE%\.cargo\bin\whoosh2garmin.exe sync --device edge_explore2"`

If Garmin ends the session, `sync` fails with a message asking you to run `login` again.
