package library;

public class Checkout {
    private final Book book;
    private final Member member;
    private final int dueDay;

    public Checkout(Book book, Member member, int dueDay) {
        this.book = book;
        this.member = member;
        this.dueDay = dueDay;
    }

    public Book getBook() {
        return book;
    }

    public Member getMember() {
        return member;
    }

    public int getDueDay() {
        return dueDay;
    }

    public boolean isOverdue(int today) {
        return today > dueDay;
    }
}
