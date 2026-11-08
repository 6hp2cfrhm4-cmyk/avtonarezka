import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  resetKey?: string;
  /** Если задан, вместо карточки с ошибкой показывается он (например, для фона). */
  fallback?: ReactNode;
}

export default class ErrorBoundary extends Component<Props, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidUpdate(prev: Props) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) this.setState({ error: null });
  }

  render() {
    if (!this.state.error) return this.props.children;
    if (this.props.fallback !== undefined) return this.props.fallback;
    return (
      <div className="grid h-full place-items-center">
        <div className="glass max-w-lg rounded-[28px] p-8 text-center">
          <div className="font-display text-xl font-semibold">Экран упал</div>
          <p className="mt-3 break-words text-sm text-white/60">{this.state.error.message}</p>
          <button className="btn mt-5" onClick={() => this.setState({ error: null })}>
            Попробовать снова
          </button>
        </div>
      </div>
    );
  }
}
