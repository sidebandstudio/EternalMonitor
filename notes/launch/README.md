# Launch kit

Everything needed to announce EternalMonitor yourself: posts, a shot list,
starter issues for new contributors and a community post. Nothing here is
published automatically.

| File | What it is |
| --- | --- |
| [posts.md](posts.md) | Three posts: an extra monitor for a Windows PC (r/ipad), Apple Pencil in Clip Studio Paint (r/ClipStudio), and the Rust and Swift implementation (Show HN, then r/rust). Each has a title, body, the visual to attach and notes on the venue's rules. |
| [shots.md](shots.md) | A three-shot recording to film on real devices, the site films that stand in until then, and a screenshot list. |
| [good-first-issues.md](good-first-issues.md) | Five small, real issues to paste into GitHub with the `good first issue` label before the posts go out, so new visitors have somewhere to start. |
| [discord-share-your-setup.md](discord-share-your-setup.md) | A Discord post asking people to share setups and drawings, with permission asked before featuring anything. |
| [v0.3.3-release-body.md](v0.3.3-release-body.md) | What the release workflow now publishes for a stable tag, shown for v0.3.3: install steps, the installer's SHA-256, then that version's notes from RELEASE_NOTES.md. A reference only; the published v0.3.3 release is unchanged. |
| Launch film | A 35 s, 1920 x 1080 film for the posts (posts.md, post 1) and for X and Discord: the site's hero over Wi-Fi then USB, the second-screen and Apple Pencil films, NVIDIA, AMD and Intel PCs, then free and open source and the name with eternalmonitor.dev and the GitHub address. Silent. Its source is `site-demos/launch-film.html`; render it with `LAUNCH_FILM_DIR=<folder> site-demos/render.sh launch-film` (HyperFrames 0.8.220, Node and ffmpeg), which writes `launch-film.mp4` (H.264, about 13 MB), a poster PNG and six stills to that folder, by default `site-demos/renders/launch-film/`. The video stays out of git; attach the MP4 directly. |

## Before you post

1. Open the good first issues (good-first-issues.md) so the repo has starter
   work when visitors arrive.
2. Film the shots in shots.md if you can. The site films work in the meantime.
3. Read each venue's rules on the day (posts.md has the links) and check that
   your account meets them.
4. Take a baseline growth report (below).

Post one venue a day rather than all at once, and stay in each thread for the
first few hours.

## Measuring a launch

`scripts/growth_report.py` prints stars (with the last 7 and 28 days), forks,
watchers, downloads per release and asset, and GitHub's 14-day traffic: views,
unique visitors, clones, top referrers and popular pages. It only reads, using
your `gh` login, and needs push access for the traffic part.

```bash
python3 scripts/growth_report.py                  # text
python3 scripts/growth_report.py --json > ~/growth-2026-11-03.json
```

Save a JSON report the day before the first post, then again 3 days and 7 days
after it. GitHub forgets traffic after 14 days, so a saved file is the only way
to compare a launch week with later weeks.

What to compare:

- **Stars in the last 7 days.** The plainest measure of a post that worked.
- **Referrers.** Each venue shows up by name (news.ycombinator.com, reddit.com
  and so on). The `eternalmonitor.dev` row counts GitHub page views that came
  from the website.
- **Downloads of the newest stable release.** Count per asset, so the Windows
  installer's number is people who took the next step.
- **Popular paths.** Visitors reaching SETUP.md or docs/pencil-drawing.md are
  reading about actually using it.

Clones include CI and bots, so treat that number as noise.
