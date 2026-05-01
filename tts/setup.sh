#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
# Download model files if not present
python3 -c "
import kokoro_onnx
kokoro_onnx.Kokoro('kokoro-v1.0.onnx', 'voices-v1.0.bin')
print('Model loaded successfully')
"
echo "TTS setup complete. Run: source tts/.venv/bin/activate && python tts/server.py"
