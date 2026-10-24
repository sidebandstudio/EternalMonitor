#!/bin/sh
# Spike a matrix. Usage: run_bench.sh <phase> <log dir>. Throwaway.
set -u
B=${B:-target/release/examples/vt_encode_bench}
OUT=${2:-/tmp/em-spikes}
mkdir -p "$OUT"
run() { label=$1; shift; echo "== $label" >>"$OUT/$PHASE.log"; "$B" --label "$label" "$@" >>"$OUT/$PHASE.log" 2>&1; grep -E "^(RESULT|REOPEN|bitstream)" "$OUT/$PHASE.log" | tail -4 | grep "label=$label" ; }
PHASE=$1
case $PHASE in
throughput)
  for size in inner outer; do
    for codec in hevc h264; do
      for pix in nv12 bgra; do
        run "tp-$codec-$size-$pix-host" --codec $codec --size $size --pix $pix --fps 120 --mode throughput --seconds 8
      done
      run "tp-$codec-$size-nv12-rt0" --codec $codec --size $size --pix nv12 --fps 120 --mode throughput --seconds 8 --realtime 0
      run "tp-$codec-$size-nv12-rt1" --codec $codec --size $size --pix nv12 --fps 120 --mode throughput --seconds 8 --realtime 1
      run "tp-$codec-$size-nv12-rt0-speed" --codec $codec --size $size --pix nv12 --fps 120 --mode throughput --seconds 8 --realtime 0 --prio_speed 1
      run "tp-$codec-$size-nv12-rt1-speed" --codec $codec --size $size --pix nv12 --fps 120 --mode throughput --seconds 8 --realtime 1 --prio_speed 1
    done
    run "tp-h264-$size-nv12-lowdelay" --codec h264 --size $size --pix nv12 --fps 120 --mode throughput --seconds 8 --low_delay 1
    run "tp-h264-$size-nv12-lowdelay-rt1" --codec h264 --size $size --pix nv12 --fps 120 --mode throughput --seconds 8 --low_delay 1 --realtime 1
  done
  run "tp-hevc-inner-nv12-sw" --codec hevc --size inner --pix nv12 --fps 120 --mode throughput --seconds 4 --allow_sw 1 --require_sw 1
  ;;
paced)
  K="--keyframe_every 2 --window 10"
  run "pc-hevc-inner-60-host" --codec hevc --size inner --fps 60 --seconds 30 $K
  run "pc-hevc-inner-60-hostdrain" --codec hevc --size inner --fps 60 --seconds 30 --drain host $K
  run "pc-hevc-inner-60-bgra-mpeg" --codec hevc --size inner --fps 60 --seconds 30 --pix bgra --range mpeg $K
  run "pc-hevc-inner-60-rt0-speed" --codec hevc --size inner --fps 60 --seconds 30 --realtime 0 --prio_speed 1 $K
  run "pc-hevc-inner-120-host" --codec hevc --size inner --fps 120 --seconds 30 $K
  run "pc-hevc-inner-120-rt0-speed" --codec hevc --size inner --fps 120 --seconds 30 --realtime 0 --prio_speed 1 $K
  run "pc-hevc-inner-120-rt1-speed" --codec hevc --size inner --fps 120 --seconds 30 --prio_speed 1 $K
  run "pc-hevc-outer-120-host" --codec hevc --size outer --fps 120 --seconds 30 $K
  run "pc-hevc-outer-60-host" --codec hevc --size outer --fps 60 --seconds 30 $K
  run "pc-h264-inner-60-host" --codec h264 --size inner --fps 60 --seconds 30 $K
  run "pc-h264-inner-60-l52" --codec h264 --size inner --fps 60 --seconds 30 --level 5.2 $K
  run "pc-h264-inner-120-l52" --codec h264 --size inner --fps 120 --seconds 30 --level 5.2 $K
  run "pc-h264-inner-120-host" --codec h264 --size inner --fps 120 --seconds 30 $K
  run "pc-h264-inner-60-lowdelay" --codec h264 --size inner --fps 60 --seconds 30 --low_delay 1 $K
  run "pc-h264-outer-120-host" --codec h264 --size outer --fps 120 --seconds 30 $K
  run "pc-hevc-inner-120-expfps" --codec hevc --size inner --fps 120 --seconds 30 --expected_fps 1 $K
  run "pc-h264-inner-60-expfps" --codec h264 --size inner --fps 60 --seconds 30 --expected_fps 1 $K
  run "pc-h264-inner-60-bgra-mpeg" --codec h264 --size inner --fps 60 --seconds 30 --pix bgra --range mpeg $K
  ;;
reopen)
  run "ro-hevc-inner-60" --codec hevc --size inner --fps 60 --mode reopen --reopen_every 2 --seconds 30 --window 10
  run "ro-h264-inner-60" --codec h264 --size inner --fps 60 --mode reopen --reopen_every 2 --seconds 30 --window 10
  run "ro-hevc-inner-120" --codec hevc --size inner --fps 120 --mode reopen --reopen_every 2 --seconds 30 --window 10
  ;;
soak60)
  run "soak-hevc-inner-60-host" --codec hevc --size inner --fps 60 --seconds 630 --keyframe_every 5 --window 30
  ;;
soak120)
  run "soak-hevc-inner-120-rt0-speed" --codec hevc --size inner --fps 120 --seconds 630 --realtime 0 --prio_speed 1 --keyframe_every 5 --window 30
  ;;
gop)
  K="--keyframe_every 2 --window 10 --seconds 20"
  run "gop-ff-hevc-inner-60-gop30" --codec hevc --size inner --fps 60 --gop 30 $K
  run "gop-ff-hevc-inner-60-gop600" --codec hevc --size inner --fps 60 --gop 600 $K
  run "gop-ff-hevc-inner-60-gop100k" --codec hevc --size inner --fps 60 --gop 100000 $K
  run "gop-ff-hevc-inner-120-gop100k" --codec hevc --size inner --fps 120 --gop 100000 $K
  run "gop-ff-h264-inner-60-l52-gop100k" --codec h264 --size inner --fps 60 --level 5.2 --gop 100000 $K
  ;;
bgra)
  K="--keyframe_every 2 --window 10 --seconds 20"
  run "bgra-hevc-inner-60-jpeg" --codec hevc --size inner --fps 60 --pix bgra --range jpeg $K
  run "bgra-hevc-inner-120-jpeg" --codec hevc --size inner --fps 120 --pix bgra --range jpeg $K
  run "bgra-h264-inner-60-jpeg" --codec h264 --size inner --fps 60 --pix bgra --range jpeg $K
  ;;
gopvt)
  B=${D:-target/release/examples/vt_direct_session}
  run "gop-vt-hevc-inner-60-gop30" --codec hevc --size inner --fps 60 --gop 30 --seconds 20 --keyframe_every 2
  run "gop-vt-hevc-inner-60-gop100k" --codec hevc --size inner --fps 60 --seconds 20 --keyframe_every 2
  run "gop-vt-hevc-inner-120-gop100k" --codec hevc --size inner --fps 120 --seconds 20 --keyframe_every 2
  run "re-vt-h264-inner-60-speed" --codec h264 --size inner --fps 60 --seconds 10 --prio_speed 1 --keyframe_every 1
  run "re-vt-h264-inner-60-bgra" --codec h264 --size inner --fps 60 --seconds 10 --pix bgra --keyframe_every 1
  run "re-vt-h264-outer-120-llrc0" --codec h264 --size outer --fps 120 --seconds 10 --keyframe_every 1
  ;;
direct)
  B=${D:-target/release/examples/vt_direct_session}
  for codec in hevc h264; do
    for llrc in 0 1; do
      for fps in 60 120; do
        run "vt-$codec-inner-$fps-llrc$llrc" --codec $codec --size inner --fps $fps --llrc $llrc --seconds 10 --keyframe_every 1
      done
      run "vt-$codec-inner-tp-llrc$llrc" --codec $codec --size inner --fps 120 --llrc $llrc --seconds 8 --mode throughput
    done
    run "vt-$codec-inner-60-delay0" --codec $codec --size inner --fps 60 --seconds 10 --max_delay 0 --keyframe_every 1
    run "vt-$codec-inner-60-rt0" --codec $codec --size inner --fps 60 --seconds 10 --realtime 0 --keyframe_every 1
    run "vt-$codec-inner-60-speed" --codec $codec --size inner --fps 60 --seconds 10 --prio_speed 1 --keyframe_every 1
    run "vt-$codec-inner-60-bgra" --codec $codec --size inner --fps 60 --seconds 10 --pix bgra --keyframe_every 1
    run "vt-$codec-outer-120-llrc0" --codec $codec --size outer --fps 120 --seconds 10 --keyframe_every 1
    run "vt-$codec-outer-120-llrc1" --codec $codec --size outer --fps 120 --seconds 10 --llrc 1 --keyframe_every 1
    for llrc in 0 1; do
      run "vt-$codec-inner-60-live-llrc$llrc" --codec $codec --size inner --fps 60 --llrc $llrc --seconds 12 --change_at 6 --change_mbps 40
    done
  done
  ;;
esac
