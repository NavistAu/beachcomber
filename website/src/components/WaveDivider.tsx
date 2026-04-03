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
        viewBox="0 0 1440 72"
        xmlns="http://www.w3.org/2000/svg"
        preserveAspectRatio="none"
        style={{display: 'block', width: '100%', height: '72px'}}
      >
        <path
          d="M0,36 C120,62 240,10 400,38 C560,66 640,14 800,36 C960,58 1080,8 1200,32 C1320,56 1390,22 1440,28 L1440,72 L0,72 Z"
          fill={fillColor}
        />
      </svg>
    </div>
  );
}
