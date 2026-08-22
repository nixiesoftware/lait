export async function getFileDimensions(file: File): Promise<{ width: number; height: number } | null> {
  return new Promise((resolve) => {
    if (file.type.startsWith('image/')) {
      const img = new Image();
      const url = URL.createObjectURL(file);
      img.onload = () => {
        URL.revokeObjectURL(url);
        resolve({ width: img.width, height: img.height });
      };
      img.onerror = () => {
        URL.revokeObjectURL(url);
        resolve(null);
      };
      img.src = url;
    } else if (file.type.startsWith('video/')) {
      const video = document.createElement("video");
      const url = URL.createObjectURL(file);
      video.muted = true;
      video.preload = "metadata";

      const cleanup = () => {
        video.pause();
        video.removeAttribute('src');
        video.load();
        video.remove();
        URL.revokeObjectURL(url);
      };

      video.onloadedmetadata = () => {
        const dimensions = { width: video.videoWidth, height: video.videoHeight };
        cleanup();
        resolve(dimensions);
      };
      video.onerror = () => {
        cleanup();
        resolve(null);
      };
      video.src = url;
    } else {
      resolve(null);
    }
  });
};
