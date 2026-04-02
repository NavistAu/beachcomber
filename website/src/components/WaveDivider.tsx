interface WaveDividerProps {
  fillColor: string;
  className?: string;
  flip?: boolean;
}

export default function WaveDivider({fillColor, className, flip = false}: WaveDividerProps): JSX.Element {
  return (
    <div
      className={className}
      style={{
        lineHeight: 0,
        overflow: 'hidden',
        transform: flip ? 'scaleX(-1)' : undefined,
      }}
    >
      <svg
        viewBox="0 0 1440 80"
        xmlns="http://www.w3.org/2000/svg"
        preserveAspectRatio="none"
        style={{display: 'block', width: '100%', height: '80px'}}
      >
        <path
          d="M0,40 C180,80 360,0 540,40 C720,80 900,0 1080,40 C1260,80 1380,20 1440,30 L1440,80 L0,80 Z"
          fill={fillColor}
        />
      </svg>
    </div>
  );
}
