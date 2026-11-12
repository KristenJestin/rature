#!/usr/bin/env bash
# Writes a canvas of <count> text cards, for measuring the board: a grid of cards of several
# kinds of Markdown (headings, emphasis, lists, a quote, code), every colour, and a group around
# each band of four rows. The data is invented.
# usage: tools/canvas-gen.sh <count> <out.canvas>
set -eu
count=$1; out=$2
W=400; H=260; GAP=60
# About three columns for two rows.
cols=1
while [ $((cols * cols * 2)) -lt $((count * 3)) ]; do cols=$((cols + 1)); done
rows=$(( (count + cols - 1) / cols ))
colors=('' '"color":"1",' '"color":"2",' '"color":"3",' '"color":"4",' '"color":"5",' '"color":"6",' '"color":"#4a90d9",')

text() {
  local i=$1 m=$(( $1 % 12 + 1 )) w=$(( $1 * 7 % 40 + 3 ))
  case $(( $1 % 5 )) in
    0) printf '# Station %d\\n\\nWind **%d km/h**, gusts to %d.\\n\\n- Temperature: %d °C\\n- Humidity: %d %%' \
         "$i" "$w" $(( w + 9 )) $(( i % 30 - 5 )) $(( i * 3 % 60 + 30 )) ;;
    1) printf '## Reading %d\\n\\nThe sensor on mast %d reports a *steady* pressure of %d hPa, and the logger kept every minute of it.' \
         "$i" "$m" $(( 990 + i % 40 )) ;;
    2) printf '### Check %d\\n\\n1. Clean the rain gauge\\n2. Read the logger\\n3. Note the time on mast %d' "$i" "$m" ;;
    3) printf 'Note %d: the anemometer needs `calibrate --mast %d` before the next run.\\n\\n> Spare bearings are in the shed.' "$i" "$m" ;;
    4) printf '#### Log %d\\n\\n```sh\\nlogger read --mast %d --since 6h\\n```' "$i" "$m" ;;
  esac
}

{
  printf '{\n\t"nodes":['
  sep='\n\t\t'
  for (( band = 0; band * 4 < rows; band++ )); do
    first=$(( band * 4 )); last=$(( first + 3 < rows - 1 ? first + 3 : rows - 1 ))
    printf "$sep"'{"id":"group%04d","type":"group","label":"Masts, rows %d to %d",%s"x":%d,"y":%d,"width":%d,"height":%d}' \
      "$band" $(( first + 1 )) $(( last + 1 )) "${colors[$(( band % 8 ))]}" \
      $(( -GAP / 2 )) $(( first * (H + GAP) - GAP / 2 )) \
      $(( cols * (W + GAP) )) $(( (last - first + 1) * (H + GAP) ))
    sep=',\n\t\t'
  done
  for (( i = 0; i < count; i++ )); do
    printf "$sep"'{"id":"card%05d","type":"text","text":"%s",%s"x":%d,"y":%d,"width":%d,"height":%d}' \
      "$i" "$(text "$i")" "${colors[$(( i % 8 ))]}" \
      $(( i % cols * (W + GAP) )) $(( i / cols * (H + GAP) )) "$W" "$H"
    sep=',\n\t\t'
  done
  printf '\n\t],\n\t"edges":[]\n}'
} >"$out"
echo "wrote $out: $count cards in $cols columns and $rows rows"
