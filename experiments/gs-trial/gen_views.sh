#!/usr/bin/env bash
# #148 第 2 轮:让 Codex(gpt-image)按用户认可的站立三视图补画三个视角,每个视角出几张候选。
# 用法(pc1,出图是云端调用): bash gen_views.sh <三视图目录> <输出目录> [每视角张数=3] [视角...]
#   视角: front34(前方四分之三略俯)、back34(后方另一侧四分之三)、top(正上方俯视)
# 产物: <输出目录>/<视角>-<序号>.png,和 prompts.txt(发给 Codex 的原文)
# -i 吃多个值,提示词必须放在它前面,否则会被当成图片路径。
# 出图方式照 anki skill 的 reference/media.md:codex exec 的退出码不可信,只认 ~/.codex/generated_images 下有没有新文件。
set -euo pipefail
REF=$1
OUT=$2
N=${3:-3}
shift $(($# < 3 ? $# : 3))
VIEWS=("$@")
[ ${#VIEWS[@]} -gt 0 ] || VIEWS=(front34 back34 top)
mkdir -p "$OUT"
CODEX=$(command -v codex)       # codex exec 之后 PATH 可能被清空(media.md 观察到过一次),先存绝对路径

declare -A ANGLE CANVAS
ANGLE[front34]="a front three-quarter view from slightly above: the camera is about 45 degrees around from the front toward the cat's left side (the same side that the side-view reference shows) and about 20 degrees above the cat, looking slightly down. The head is on the left half of the picture, turned three-quarters toward the viewer so both eyes are visible; the body recedes to the right and the tail rises behind it on the right"
CANVAS[front34]="wide 3:2 landscape frame"
ANGLE[back34]="a rear three-quarter view: the camera is about 45 degrees around from directly behind toward the cat's right side (the side the side-view reference does NOT show), a little above the cat's back. We see the back, the rump and the right flank; the head is at the far right of the picture facing away from the viewer so only the back of the head, the ears and a sliver of the right cheek show; the big tail extends backward toward the viewer on the left"
CANVAS[back34]="wide 3:2 landscape frame"
ANGLE[top]="a view from directly above, looking straight down at the cat's back like a top-down orthographic drawing. The head points to the top of the picture and the tail to the bottom; we see the top of the head with both ears, the shoulders, the spine, the rump, the tail spread out behind, and the four white paws peeking out at the sides"
CANVAS[top]="tall 2:3 portrait frame"

prompt() {
  cat <<EOF
Call the image_generation tool RIGHT NOW as your very first action. Do not read any files.
Do not load any skill. Do not run any shell command. Do not explain. Use the three attached
images as the character and style reference and call image_generation once with the prompt
below, then stop.

${CANVAS[$1]}. The three attached images are an approved character turnaround of one
long-haired tuxedo cat standing: a side view, a front view and a back view. Draw this same
individual cat from a new camera angle: ${ANGLE[$1]}.

Keep the exact standing pose of the turnaround: all four legs straight and planted, the head
level and looking forward, the big plume tail extended backward and curving upward. Keep the
exact markings: a fully black face, a white chin that joins a white bib running down the
chest, four white paws like gloves, yellow-green eyes, black ears with pinkish brown insides,
long black fur with warm dark-brown sheen, a very full fluffy tail. Keep the same body
proportions as the turnaround.

Paint it in the same style as the references: a detailed semi-realistic anime illustration,
fur painted as layered tufts with sharp pointed tips, soft even lighting. The whole cat is
visible and fills the frame with only a small margin, on a plain pure white background with
no floor and no shadow.

Avoid a sitting, walking or crouching pose, extra or missing legs, white on the face above
the chin, a thin or short tail, a cast shadow, any text, and any background scene.
EOF
}

newest() { find ~/.codex/generated_images -type f -name '*.png' -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1; }

for v in "${VIEWS[@]}"; do
  { echo "===== $v"; prompt "$v"; } >> "$OUT/prompts.txt"
  for i in $(seq 1 "$N"); do
    for try in 1 2; do                 # 没出新文件就重试一次
      before=$(newest)
      t=$(date +%s)
      (cd "$OUT" && "$CODEX" exec --skip-git-repo-check "$(prompt "$v")" \
        -i "$REF/side.png" "$REF/front.png" "$REF/back2.png" < /dev/null > "$OUT/$v-$i.log" 2>&1) || true
      after=$(newest)
      if [ -n "$after" ] && [ "$after" != "$before" ]; then
        cp "${after#* }" "$OUT/$v-$i.png"
        echo "GEN $v-$i $(( $(date +%s) - t ))s"
        break
      fi
      echo "NOFILE $v-$i try $try"
    done
  done
done
echo GEN_DONE
