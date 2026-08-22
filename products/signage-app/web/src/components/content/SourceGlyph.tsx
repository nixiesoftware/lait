import React from "react";
import { File, LayoutTemplate, Puzzle, Radio } from "lucide-react";
import { BsImageFill } from "react-icons/bs";
import { PiVideoCameraFill } from "react-icons/pi";
import type { MediaSource } from "@/utils/lait/types";

export const SourceGlyph: React.FC<{ source: MediaSource; className?: string }> = ({
  source,
  className = "w-3 h-3",
}) => {
  switch (source.source) {
    case "stored":
      if (source.mime.startsWith("image")) return <BsImageFill className={className} />;
      if (source.mime.startsWith("video")) return <PiVideoCameraFill className={className} />;
      return <File className={className} />;
    case "card":
      return <LayoutTemplate className={className} />;
    case "kind":
      return <Puzzle className={className} />;
    case "live":
      return <Radio className={className} />;
  }
};
