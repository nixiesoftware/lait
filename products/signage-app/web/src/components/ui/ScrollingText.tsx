import React, { useRef, useState, useEffect } from "react";

interface ScrollingTextProps {
  text: string;
  className?: string;
  speed?: number; // pixels per second
  delay?: number; // delay before scrolling starts in ms
  isParentHovered?: boolean; // Control hover state from parent
}

export const ScrollingText: React.FC<ScrollingTextProps> = ({
  text,
  className = "",
  speed = 40,
  delay = 500,
  isParentHovered = false,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const textRef = useRef<HTMLSpanElement>(null);
  const [isOverflowing, setIsOverflowing] = useState(false);
  const [scrollDistance, setScrollDistance] = useState(0);

  useEffect(() => {
    const checkOverflow = () => {
      if (containerRef.current && textRef.current) {
        const containerWidth = containerRef.current.offsetWidth;
        const textWidth = textRef.current.scrollWidth;
        
        if (textWidth > containerWidth) {
          setIsOverflowing(true);
          // Add extra space at the end for readability
          setScrollDistance(textWidth - containerWidth + 30);
        } else {
          setIsOverflowing(false);
          setScrollDistance(0);
        }
      }
    };

    // Check after a small delay to ensure proper rendering
    const timer = setTimeout(checkOverflow, 10);
    
    // Recheck on resize
    window.addEventListener('resize', checkOverflow);
    
    return () => {
      clearTimeout(timer);
      window.removeEventListener('resize', checkOverflow);
    };
  }, [text]);

  const animationDuration = scrollDistance > 0 ? scrollDistance / speed : 0;

  return (
    <div
      ref={containerRef}
      className={`relative w-full overflow-hidden ${className}`}
    >
      <div
        className={`${isOverflowing && !isParentHovered ? 'truncate' : ''}`}
      >
        <span
          ref={textRef}
          className={`inline-block whitespace-nowrap ${
            isOverflowing && isParentHovered ? 'animate-scroll-text' : ''
          }`}
          style={{
            animationDuration: `${animationDuration}s`,
            animationDelay: `${delay}ms`,
            '--scroll-distance': `-${scrollDistance}px`,
          } as React.CSSProperties}
        >
          {text}
        </span>
      </div>

      <style>{`
        @keyframes scroll-text {
          0% {
            transform: translateX(0);
          }
          80% {
            transform: translateX(var(--scroll-distance));
          }
          100% {
            transform: translateX(var(--scroll-distance));
          }
        }

        .animate-scroll-text {
          animation-name: scroll-text;
          animation-timing-function: linear;
          animation-fill-mode: forwards;
        }
      `}</style>
    </div>
  );
};