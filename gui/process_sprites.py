"""Process sprite images: remove magenta background, create animation frames."""
from PIL import Image
import numpy as np
from pathlib import Path

ASSETS = Path("assets")

def remove_magenta_bg(input_path, output_path, threshold=80):
    """Remove magenta (#FF00FF) background and save as PNG with transparency."""
    img = Image.open(input_path).convert("RGBA")
    data = np.array(img)

    # Magenta: high R, low G, high B
    r, g, b, a = data[:,:,0], data[:,:,1], data[:,:,2], data[:,:,3]
    magenta_mask = (r > 180) & (g < threshold) & (b > 180)

    # Also remove near-magenta pixels (anti-aliasing artifacts)
    near_magenta = (r > 150) & (g < 100) & (b > 150)

    data[magenta_mask, 3] = 0  # Fully transparent
    data[near_magenta, 3] = 0  # Remove near-magenta too

    result = Image.fromarray(data)
    result.save(output_path)
    print(f"  Saved: {output_path} ({result.size[0]}x{result.size[1]})")
    return result

def create_walk_frames(sprite_path, output_prefix, num_frames=4, flip=False):
    """Create simple walk animation frames by slightly modifying the sprite."""
    img = Image.open(sprite_path).convert("RGBA")
    w, h = img.size

    frames = []
    for i in range(num_frames):
        frame = img.copy()

        if i == 1:
            # Slight shift right (walk step 1)
            shifted = Image.new("RGBA", (w, h), (0, 0, 0, 0))
            shifted.paste(frame, (2, 0))
            frame = shifted
        elif i == 2:
            # Bob up slightly (mid-step)
            shifted = Image.new("RGBA", (w, h), (0, 0, 0, 0))
            shifted.paste(frame, (0, -3))
            frame = shifted
        elif i == 3:
            # Slight shift left (walk step 2)
            shifted = Image.new("RGBA", (w, h), (0, 0, 0, 0))
            shifted.paste(frame, (-2, 0))
            frame = shifted

        if flip:
            frame = frame.transpose(Image.FLIP_LEFT_RIGHT)

        out_path = f"{output_prefix}_{i}.png"
        frame.save(out_path)
        frames.append(out_path)

    print(f"  Created {num_frames} animation frames: {output_prefix}_*.png")
    return frames

def create_spritesheet(frames_paths, output_path):
    """Combine individual frames into a horizontal sprite sheet."""
    frames = [Image.open(p).convert("RGBA") for p in frames_paths]
    w, h = frames[0].size
    sheet = Image.new("RGBA", (w * len(frames), h), (0, 0, 0, 0))
    for i, frame in enumerate(frames):
        sheet.paste(frame, (i * w, 0))
    sheet.save(output_path)
    print(f"  Spritesheet: {output_path} ({sheet.size[0]}x{sheet.size[1]})")

def main():
    print("Processing sprites...")

    # Process hero (user character)
    print("\n[User Hero]")
    hero = remove_magenta_bg(ASSETS / "hero_user.jpg", ASSETS / "hero_user.png")
    user_frames = create_walk_frames(ASSETS / "hero_user.png", str(ASSETS / "hero_user_walk"))
    create_spritesheet(user_frames, ASSETS / "hero_user_sheet.png")

    # Process bot character
    print("\n[AI Bot]")
    bot = remove_magenta_bg(ASSETS / "hero_bot.jpg", ASSETS / "hero_bot.png")
    bot_frames = create_walk_frames(ASSETS / "hero_bot.png", str(ASSETS / "hero_bot_walk"), flip=False)
    create_spritesheet(bot_frames, ASSETS / "hero_bot_sheet.png")

    # Resize sprites to consistent game size (64x64)
    print("\n[Resizing to 64x64]")
    for name in ["hero_user.png", "hero_bot.png"]:
        img = Image.open(ASSETS / name).convert("RGBA")
        resized = img.resize((64, 64), Image.NEAREST)  # Nearest neighbor for pixel art
        resized.save(ASSETS / name)
        print(f"  Resized {name} to 64x64")

    for prefix in ["hero_user_walk", "hero_bot_walk"]:
        for i in range(4):
            path = ASSETS / f"{prefix}_{i}.png"
            img = Image.open(path).convert("RGBA")
            resized = img.resize((64, 64), Image.NEAREST)
            resized.save(path)
        print(f"  Resized {prefix}_*.png to 64x64")

    # Recreate spritesheets at 64x64
    for prefix, sheet_name in [("hero_user_walk", "hero_user_sheet.png"),
                                ("hero_bot_walk", "hero_bot_sheet.png")]:
        paths = [str(ASSETS / f"{prefix}_{i}.png") for i in range(4)]
        create_spritesheet(paths, ASSETS / sheet_name)

    print("\nDone!")

if __name__ == "__main__":
    main()
