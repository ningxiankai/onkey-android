"""Export a torchcrepe model (MIT) to ONNX: python export_crepe.py full|tiny OUT.onnx [BATCH]
With BATCH the batch size is fixed (Core ML needs fixed shapes); without it, any number of frames.
Needs torch, torchcrepe and onnx. Input "frames" (n, 1024): 64 ms windows at 16 kHz, each normalised to zero
mean and unit variance; output "probs" (n, 360): pitch-bin activations, 20 cents apart from 31.7 Hz."""
import sys
from pathlib import Path
import torch, torchcrepe
cap, out = sys.argv[1], sys.argv[2]
m = torchcrepe.Crepe(cap)
m.load_state_dict(torch.load(Path(torchcrepe.__file__).parent / "assets" / f"{cap}.pth", map_location="cpu", weights_only=True))
m.eval()
batch = int(sys.argv[3]) if len(sys.argv) > 3 else None
torch.onnx.export(m, (torch.randn(batch or 8, 1024),), out, input_names=["frames"], output_names=["probs"],
                  dynamic_axes=None if batch else {"frames": {0: "n"}, "probs": {0: "n"}}, opset_version=13, dynamo=False)
print("exported", out)
