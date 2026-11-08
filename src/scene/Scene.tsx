import { Canvas, useFrame, useThree } from "@react-three/fiber";
import { useEffect, useMemo, useRef, useState } from "react";
import * as THREE from "three";
import ErrorBoundary from "../components/ErrorBoundary";
import { useUi, type ScreenId } from "../store";

/** Положение камеры для каждого шага: сцена «переезжает» вместе с интерфейсом. */
const POSES: Record<ScreenId, { pos: [number, number, number]; look: [number, number, number] }> = {
  import: { pos: [0, 0.2, 9], look: [0, 0, 0] },
  analysis: { pos: [-3.5, 1.2, 7.5], look: [0, 0, -1] },
  moments: { pos: [3.2, -0.6, 7], look: [0, 0.2, -1] },
  drafts: { pos: [0, 2.2, 8.5], look: [0, 0, -1] },
  final: { pos: [0, -0.4, 6], look: [0, 0.4, -2] },
};

function roundedRect(w: number, h: number, r: number): THREE.Shape {
  const s = new THREE.Shape();
  const x = -w / 2;
  const y = -h / 2;
  s.moveTo(x + r, y);
  s.lineTo(x + w - r, y);
  s.quadraticCurveTo(x + w, y, x + w, y + r);
  s.lineTo(x + w, y + h - r);
  s.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
  s.lineTo(x + r, y + h);
  s.quadraticCurveTo(x, y + h, x, y + h - r);
  s.lineTo(x, y + r);
  s.quadraticCurveTo(x, y, x + r, y);
  return s;
}

/** Парящие рамки 9:16 — те самые вертикальные ролики. */
function Frames({ accent }: { accent: THREE.Color }) {
  const group = useRef<THREE.Group>(null);
  const shape = useMemo(() => roundedRect(1.08, 1.92, 0.14), []);
  const fillGeo = useMemo(() => new THREE.ShapeGeometry(shape, 8), [shape]);
  const lineGeo = useMemo(() => new THREE.BufferGeometry().setFromPoints(shape.getPoints(10)), [shape]);

  const items = useMemo(
    () =>
      Array.from({ length: 9 }, (_, i) => {
        const a = (i / 9) * Math.PI * 2 + 0.4;
        const radius = 6.2 + (i % 3) * 1.3;
        return {
          x: Math.cos(a) * radius,
          y: ((i * 37) % 11) / 11 * 4 - 2,
          z: Math.sin(a) * radius * 0.55 - 3.5,
          ry: -a * 0.35,
          scale: 0.7 + ((i * 13) % 7) / 10,
          speed: 0.25 + ((i * 7) % 5) / 12,
          phase: i * 1.7,
          hero: i === 2,
        };
      }),
    [],
  );

  useFrame((state) => {
    const t = state.clock.elapsedTime;
    group.current?.children.forEach((child, i) => {
      const it = items[i];
      child.position.y = it.y + Math.sin(t * it.speed + it.phase) * 0.35;
      child.rotation.y = it.ry + Math.sin(t * 0.2 + it.phase) * 0.12;
      child.rotation.z = Math.sin(t * 0.15 + it.phase) * 0.04;
    });
  });

  return (
    <group ref={group}>
      {items.map((it, i) => (
        <group key={i} position={[it.x, it.y, it.z]} scale={it.scale}>
          <mesh geometry={fillGeo}>
            <meshBasicMaterial
              color={it.hero ? accent : "#7d78ff"}
              transparent
              opacity={it.hero ? 0.07 : 0.03}
              side={THREE.DoubleSide}
              depthWrite={false}
            />
          </mesh>
          <lineLoop geometry={lineGeo}>
            <lineBasicMaterial color={it.hero ? accent : "#9a96ff"} transparent opacity={it.hero ? 0.6 : 0.24} />
          </lineLoop>
          {/* линия раздела верх/низ — как в готовом ролике */}
          <mesh position={[0, 0, 0.001]}>
            <planeGeometry args={[1.0, 0.012]} />
            <meshBasicMaterial color={it.hero ? accent : "#9a96ff"} transparent opacity={it.hero ? 0.5 : 0.16} />
          </mesh>
        </group>
      ))}
    </group>
  );
}

function Dust({ accent }: { accent: THREE.Color }) {
  const ref = useRef<THREE.Points>(null);
  const positions = useMemo(() => {
    const n = 650;
    const arr = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) {
      arr[i * 3] = (Math.random() - 0.5) * 24;
      arr[i * 3 + 1] = (Math.random() - 0.5) * 14;
      arr[i * 3 + 2] = -Math.random() * 14 + 3;
    }
    return arr;
  }, []);

  useFrame((_, dt) => {
    if (ref.current) {
      ref.current.rotation.y += dt * 0.01;
      ref.current.rotation.x += dt * 0.003;
    }
  });

  return (
    <points ref={ref}>
      <bufferGeometry>
        <bufferAttribute attach="attributes-position" args={[positions, 3]} />
      </bufferGeometry>
      <pointsMaterial size={0.035} color={accent} transparent opacity={0.55} sizeAttenuation depthWrite={false} />
    </points>
  );
}

/** Камера плавно едет к позе текущего шага + параллакс от мыши. */
function Rig() {
  const screen = useUi((s) => s.screen);
  const { camera, pointer } = useThree();
  const look = useRef(new THREE.Vector3());

  useFrame((_, dt) => {
    const pose = POSES[screen];
    const k = 1 - Math.exp(-dt * 2.2);
    camera.position.x += (pose.pos[0] + pointer.x * 0.7 - camera.position.x) * k;
    camera.position.y += (pose.pos[1] + pointer.y * 0.45 - camera.position.y) * k;
    camera.position.z += (pose.pos[2] - camera.position.z) * k;
    look.current.x += (pose.look[0] - look.current.x) * k;
    look.current.y += (pose.look[1] - look.current.y) * k;
    look.current.z += (pose.look[2] - look.current.z) * k;
    camera.lookAt(look.current);
  });
  return null;
}

let webglOk: boolean | null = null;
function hasWebGL(): boolean {
  if (webglOk === null) {
    try {
      const c = document.createElement("canvas");
      webglOk = !!(c.getContext("webgl2") || c.getContext("webgl"));
    } catch {
      webglOk = false;
    }
  }
  return webglOk;
}

/** Запасной фон, если WebGL в окне недоступен: приложение остаётся рабочим. */
function CssBackdrop() {
  return (
    <div
      className="pointer-events-none fixed inset-0"
      style={{
        background:
          "radial-gradient(circle at 18% 22%, color-mix(in srgb, var(--accent) 13%, transparent), transparent 42%), radial-gradient(circle at 82% 72%, rgba(139,108,255,0.2), transparent 46%), #06060b",
      }}
    />
  );
}

export default function Scene() {
  if (!hasWebGL()) return <CssBackdrop />;
  return (
    <ErrorBoundary fallback={<CssBackdrop />}>
      <Scene3D />
    </ErrorBoundary>
  );
}

function Scene3D() {
  const accentHex = useUi((s) => s.accent);
  const lite = useUi((s) => s.lite);
  const screen = useUi((s) => s.screen);
  const accent = useMemo(() => new THREE.Color(accentHex), [accentHex]);

  // На экранах с сеткой карточек фон замирает, когда камера доехала: иначе композитор пересобирает окно
  // с сотней слоёв 60 раз в секунду ради фона, на который никто не смотрит.
  const heavy = screen === "drafts" || screen === "final";
  const [settled, setSettled] = useState(false);
  useEffect(() => {
    setSettled(false);
    if (!heavy) return;
    const t = setTimeout(() => setSettled(true), 2500);
    return () => clearTimeout(t);
  }, [screen, heavy]);

  return (
    <div className="pointer-events-none fixed inset-0 -z-0">
      <Canvas
        dpr={[1, 1.5]}
        camera={{ position: [0, 0.2, 9], fov: 48, near: 0.1, far: 60 }}
        frameloop={lite || (heavy && settled) ? "demand" : "always"}
        gl={{ antialias: true, alpha: true, powerPreference: "high-performance" }}
      >
        <fog attach="fog" args={["#06060b", 8, 22]} />
        <Frames accent={accent} />
        <Dust accent={accent} />
        <Rig />
      </Canvas>
      <div
        className="absolute inset-0"
        style={{
          background:
            "radial-gradient(ellipse 70% 60% at 50% 40%, transparent 0%, rgba(6,6,11,0.55) 70%, rgba(6,6,11,0.92) 100%)",
        }}
      />
    </div>
  );
}
